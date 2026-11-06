use core::mem;

use ironrdp_core::WriteBuf;
use ironrdp_pdu::PduHint;
use ironrdp_pdu::rdp::capability_sets::SERVER_CHANNEL_ID;
use ironrdp_pdu::rdp::headers::ShareDataPdu;
use ironrdp_pdu::rdp::{finalization_messages, server_error_info};
use tracing::{debug, warn};

use crate::{
    ConnectorError, ConnectorErrorExt as _, ConnectorResult, MonotonicInstant, Sequence, State, Written, general_err,
    reason_err,
};

#[derive(Default, Debug, Copy, Clone)]
#[non_exhaustive]
pub enum ConnectionFinalizationState {
    #[default]
    Consumed,

    SendSynchronize,
    SendControlCooperate,
    SendRequestControl,
    SendFontList,

    WaitForResponse,

    Finished,
}

impl State for ConnectionFinalizationState {
    fn name(&self) -> &'static str {
        match self {
            Self::Consumed => "Consumed",
            Self::SendSynchronize => "SendSynchronize",
            Self::SendControlCooperate => "SendControlCooperate",
            Self::SendRequestControl => "SendRequestControl",
            Self::SendFontList => "SendFontList",
            Self::WaitForResponse => "WaitForResponse",
            Self::Finished => "Finished",
        }
    }

    fn is_terminal(&self) -> bool {
        matches!(self, Self::Finished)
    }

    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}

#[derive(Debug, Clone)]
pub struct ConnectionFinalizationSequence {
    pub state: ConnectionFinalizationState,
    pub io_channel_id: u16,
    pub user_channel_id: u16,
    pub share_id: u32,
    pub monitor_layout: Option<finalization_messages::MonitorLayoutPdu>,
    /// RDP Launcher patch: desktop size to repaint (if the server supports Refresh Rect)
    /// when graphics updates had to be skipped before the Font Map PDU.
    pub repaint_area: Option<(u16, u16)>,
    skipped_graphics: bool,
}

impl ConnectionFinalizationSequence {
    pub fn new(io_channel_id: u16, user_channel_id: u16, share_id: u32) -> Self {
        Self {
            state: ConnectionFinalizationState::SendSynchronize,
            io_channel_id,
            user_channel_id,
            share_id,
            monitor_layout: None,
            repaint_area: None,
            skipped_graphics: false,
        }
    }

    /// RDP Launcher patch: after skipping early graphics, request a repaint of this area.
    #[must_use]
    pub fn with_repaint_area(mut self, area: Option<(u16, u16)>) -> Self {
        self.repaint_area = area;
        self
    }
}

impl Sequence for ConnectionFinalizationSequence {
    fn next_pdu_hint(&self) -> Option<&dyn PduHint> {
        match self.state {
            ConnectionFinalizationState::Consumed => None,
            ConnectionFinalizationState::SendSynchronize => None,
            ConnectionFinalizationState::SendControlCooperate => None,
            ConnectionFinalizationState::SendRequestControl => None,
            ConnectionFinalizationState::SendFontList => None,
            ConnectionFinalizationState::WaitForResponse => Some(&ironrdp_pdu::X224_HINT),
            ConnectionFinalizationState::Finished => None,
        }
    }

    fn state(&self) -> &dyn State {
        &self.state
    }

    fn step(
        &mut self,
        input: &[u8],
        _received_at: Option<MonotonicInstant>,
        output: &mut WriteBuf,
    ) -> ConnectorResult<Written> {
        let (written, next_state) = match mem::take(&mut self.state) {
            ConnectionFinalizationState::Consumed => {
                return Err(general_err!(
                    "connection finalization sequence state is consumed (this is a bug)",
                ));
            }

            ConnectionFinalizationState::SendSynchronize => {
                let message = ShareDataPdu::Synchronize(finalization_messages::SynchronizePdu {
                    target_user_id: self.user_channel_id,
                });

                debug!(?message, "Send");

                let written = ironrdp_pdu::rdp::headers::encode_share_data(
                    self.user_channel_id,
                    self.io_channel_id,
                    self.share_id,
                    message,
                    output,
                )
                .map_err(ConnectorError::encode)?;

                (
                    Written::from_size(written)?,
                    ConnectionFinalizationState::SendControlCooperate,
                )
            }

            ConnectionFinalizationState::SendControlCooperate => {
                let message = ShareDataPdu::Control(finalization_messages::ControlPdu {
                    action: finalization_messages::ControlAction::Cooperate,
                    grant_id: 0,
                    control_id: 0,
                });

                debug!(?message, "Send");

                let written = ironrdp_pdu::rdp::headers::encode_share_data(
                    self.user_channel_id,
                    self.io_channel_id,
                    self.share_id,
                    message,
                    output,
                )
                .map_err(ConnectorError::encode)?;

                (
                    Written::from_size(written)?,
                    ConnectionFinalizationState::SendRequestControl,
                )
            }

            ConnectionFinalizationState::SendRequestControl => {
                let message = ShareDataPdu::Control(finalization_messages::ControlPdu {
                    action: finalization_messages::ControlAction::RequestControl,
                    grant_id: 0,
                    control_id: 0,
                });

                debug!(?message, "Send");

                let written = ironrdp_pdu::rdp::headers::encode_share_data(
                    self.user_channel_id,
                    self.io_channel_id,
                    self.share_id,
                    message,
                    output,
                )
                .map_err(ConnectorError::encode)?;

                (Written::from_size(written)?, ConnectionFinalizationState::SendFontList)
            }

            ConnectionFinalizationState::SendFontList => {
                let message = ShareDataPdu::FontList(finalization_messages::FontPdu::default());

                debug!(?message, "Send");

                let written = ironrdp_pdu::rdp::headers::encode_share_data(
                    self.user_channel_id,
                    self.io_channel_id,
                    self.share_id,
                    message,
                    output,
                )
                .map_err(ConnectorError::encode)?;

                (
                    Written::from_size(written)?,
                    ConnectionFinalizationState::WaitForResponse,
                )
            }

            ConnectionFinalizationState::WaitForResponse => {
                let ctx = ironrdp_pdu::mcs::decode_send_data_indication(input).map_err(ConnectorError::decode)?;
                let ctx = ironrdp_pdu::rdp::headers::decode_share_data(ctx).map_err(ConnectorError::decode)?;

                debug!(message = ?ctx.pdu, "Received");

                let next_state = match ctx.pdu {
                    ShareDataPdu::Synchronize(_) => {
                        debug!("Server Synchronize");
                        ConnectionFinalizationState::WaitForResponse
                    }
                    ShareDataPdu::Control(control_pdu) => match control_pdu.action {
                        finalization_messages::ControlAction::Cooperate => {
                            if control_pdu.grant_id == 0 && control_pdu.control_id == 0 {
                                debug!("Server Control (Cooperate)");
                            } else {
                                warn!(
                                    control_pdu.grant_id,
                                    control_pdu.control_id,
                                    user_channel_id = self.user_channel_id,
                                    "Server Control (Cooperate) has non-zero grant_id or control_id",
                                );
                            }
                            ConnectionFinalizationState::WaitForResponse
                        }
                        finalization_messages::ControlAction::GrantedControl => {
                            debug!(
                                control_pdu.grant_id,
                                control_pdu.control_id,
                                user_channel_id = self.user_channel_id,
                                SERVER_CHANNEL_ID
                            );

                            if control_pdu.grant_id != self.user_channel_id {
                                warn!(
                                    "Server Control (Granted Control) had invalid grant_id, expected {}, but got {}",
                                    self.user_channel_id, control_pdu.grant_id
                                );
                            }

                            if control_pdu.control_id != u32::from(SERVER_CHANNEL_ID) {
                                warn!(
                                    "Server Control (Granted Control) had invalid control_id, expected {}, but got {}",
                                    SERVER_CHANNEL_ID, control_pdu.control_id
                                );
                            }

                            ConnectionFinalizationState::WaitForResponse
                        }
                        _ => return Err(general_err!("unexpected control action")),
                    },
                    ShareDataPdu::ServerSetErrorInfo(server_error_info::ServerSetErrorInfoPdu(error_info)) => {
                        match error_info {
                            server_error_info::ErrorInfo::ProtocolIndependentCode(
                                server_error_info::ProtocolIndependentCode::None,
                            ) => ConnectionFinalizationState::WaitForResponse,
                            _ => {
                                return Err(reason_err!(
                                    "ServerSetErrorInfo",
                                    "server returned error info: {}",
                                    error_info.description()
                                ));
                            }
                        }
                    }
                    ShareDataPdu::MonitorLayout(monitor_layout) => {
                        self.monitor_layout = Some(monitor_layout);
                        ConnectionFinalizationState::WaitForResponse
                    }
                    ShareDataPdu::FontMap(_) => {
                        // https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpbcgr/023f1e69-cfe8-4ee6-9ee0-7e759fb4e4ee
                        //
                        // Once the client has sent the Confirm Active PDU, it can start
                        // sending mouse and keyboard input to the server, and upon receipt
                        // of the Font List PDU the server can start sending graphics
                        // output to the client.

                        ConnectionFinalizationState::Finished
                    }
                    // RDP Launcher patch: xrdp sends graphics (and pointer) updates before its
                    // Font Map PDU during the Deactivation-Reactivation sequence it uses to
                    // resize. They are superseded by the repaint that follows; skip them rather
                    // than failing the sequence (which dropped the session on every resize).
                    ShareDataPdu::Update(_) | ShareDataPdu::Pointer(_) => {
                        debug!("Skipping graphics update received before the Font Map PDU");
                        self.skipped_graphics = true;
                        ConnectionFinalizationState::WaitForResponse
                    }
                    _ => return Err(general_err!("unexpected server message")),
                };

                // RDP Launcher patch: the skipped updates were the server's repaint; ask again.
                let written = match (&next_state, self.skipped_graphics, self.repaint_area) {
                    (ConnectionFinalizationState::Finished, true, Some((width, height))) if width > 0 && height > 0 => {
                        let message = ShareDataPdu::RefreshRectangle(
                            ironrdp_pdu::rdp::refresh_rectangle::RefreshRectanglePdu {
                                areas_to_refresh: vec![ironrdp_pdu::geometry::InclusiveRectangle {
                                    left: 0,
                                    top: 0,
                                    right: width - 1,
                                    bottom: height - 1,
                                }],
                            },
                        );
                        debug!(?message, "Send");
                        let written = ironrdp_pdu::rdp::headers::encode_share_data(
                            self.user_channel_id,
                            self.io_channel_id,
                            self.share_id,
                            message,
                            output,
                        )
                        .map_err(ConnectorError::encode)?;
                        Written::from_size(written)?
                    }
                    _ => Written::Nothing,
                };

                (written, next_state)
            }

            ConnectionFinalizationState::Finished => return Err(general_err!("finalization already finished")),
        };

        self.state = next_state;

        Ok(written)
    }
}

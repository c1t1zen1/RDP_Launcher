//! Regression test for resizing xrdp sessions: during the Deactivation-Reactivation
//! sequence xrdp sends graphics updates before its Font Map PDU. IronRDP's connection
//! finalization used to reject them ("unexpected server message") and drop the session.

use std::borrow::Cow;

use ironrdp_connector::{ConnectionFinalizationSequence, ConnectionFinalizationState, Sequence};
use ironrdp_core::{encode_vec, WriteBuf};
use ironrdp_pdu::mcs::SendDataIndication;
use ironrdp_pdu::rdp::client_info::CompressionType;
use ironrdp_pdu::rdp::finalization_messages::{ControlAction, ControlPdu, FontPdu, SynchronizePdu};
use ironrdp_pdu::rdp::headers::{
    CompressionFlags, ShareControlHeader, ShareControlPdu, ShareDataHeader, ShareDataPdu, StreamPriority,
};
use ironrdp_pdu::x224::X224;

const USER_CHANNEL: u16 = 1002;
const IO_CHANNEL: u16 = 1003;

/// A server → client share data PDU as it arrives on the wire.
fn server_pdu(pdu: ShareDataPdu) -> Vec<u8> {
    if let ShareDataPdu::Update(body) = &pdu {
        return server_update_pdu(body);
    }
    let header = ShareControlHeader {
        share_id: 0x0001_03EA,
        pdu_source: USER_CHANNEL,
        share_control_pdu: ShareControlPdu::Data(ShareDataHeader {
            share_data_pdu: pdu,
            stream_priority: StreamPriority::Undefined,
            compression_flags: CompressionFlags::empty(),
            compression_type: CompressionType::K8,
        }),
    };
    let user_data = encode_vec(&header).expect("share control header");
    encode_vec(&X224(SendDataIndication {
        initiator_id: USER_CHANNEL,
        channel_id: IO_CHANNEL,
        user_data: Cow::Owned(user_data),
    }))
    .expect("send data indication")
}

/// IronRDP can decode but not encode Update PDUs, so build one from a Synchronize PDU of
/// the same 4-byte body size: patch pduType2 (Synchronize 0x1F → Update 0x02) and the body.
fn server_update_pdu(body: &[u8]) -> Vec<u8> {
    assert_eq!(body.len(), 4, "keeps the Synchronize PDU's lengths valid");
    let header = ShareControlHeader {
        share_id: 0x0001_03EA,
        pdu_source: USER_CHANNEL,
        share_control_pdu: ShareControlPdu::Data(ShareDataHeader {
            share_data_pdu: ShareDataPdu::Synchronize(SynchronizePdu { target_user_id: USER_CHANNEL }),
            stream_priority: StreamPriority::Undefined,
            compression_flags: CompressionFlags::empty(),
            compression_type: CompressionType::K8,
        }),
    };
    let mut user_data = encode_vec(&header).expect("share control header");
    // ShareControlHeader (6) + shareId (4) + pad (1) + streamId (1) + uncompressedLength (2)
    const PDU_TYPE2: usize = 14;
    assert_eq!(user_data[PDU_TYPE2], 0x1F, "Synchronize pduType2");
    user_data[PDU_TYPE2] = 0x02;
    let body_at = user_data.len() - 4;
    user_data[body_at..].copy_from_slice(body);
    encode_vec(&X224(SendDataIndication {
        initiator_id: USER_CHANNEL,
        channel_id: IO_CHANNEL,
        user_data: Cow::Owned(user_data),
    }))
    .expect("send data indication")
}

#[test]
fn early_graphics_update_during_finalization_is_tolerated() {
    let mut seq = ConnectionFinalizationSequence::new(IO_CHANNEL, USER_CHANNEL, 0x0001_03EA);
    let mut out = WriteBuf::new();
    // Client side: Synchronize, Control (Cooperate), Control (Request), Font List.
    for _ in 0..4 {
        out.clear();
        seq.step_no_input(&mut out).expect("client finalization PDU");
    }

    // What xrdp sends after a resize: a bitmap update first, then the expected replies.
    let replies = [
        ShareDataPdu::Update(vec![1, 0, 4, 0]),
        ShareDataPdu::Synchronize(SynchronizePdu { target_user_id: USER_CHANNEL }),
        ShareDataPdu::Control(ControlPdu { action: ControlAction::Cooperate, grant_id: 0, control_id: 1002 }),
        ShareDataPdu::Control(ControlPdu { action: ControlAction::GrantedControl, grant_id: 0, control_id: 1002 }),
        ShareDataPdu::FontMap(FontPdu::default()),
    ];
    for reply in replies {
        let name = format!("{reply:?}");
        out.clear();
        seq.step(&server_pdu(reply), None, &mut out).unwrap_or_else(|e| panic!("{name}: {e}"));
    }
    assert!(matches!(seq.state, ConnectionFinalizationState::Finished));
}

/// Runs the client side of finalization, then feeds `replies`; returns the bytes the
/// sequence wrote in response to the last reply (Font Map).
fn finalize_with(seq: &mut ConnectionFinalizationSequence, replies: Vec<ShareDataPdu>) -> Vec<u8> {
    let mut out = WriteBuf::new();
    for _ in 0..4 {
        out.clear();
        seq.step_no_input(&mut out).expect("client finalization PDU");
    }
    let mut last = Vec::new();
    for reply in replies {
        out.clear();
        let written = seq.step(&server_pdu(reply), None, &mut out).expect("finalization step");
        last = written.size().map(|n| out.filled()[..n].to_vec()).unwrap_or_default();
    }
    assert!(matches!(seq.state, ConnectionFinalizationState::Finished));
    last
}

fn normal_replies() -> Vec<ShareDataPdu> {
    vec![
        ShareDataPdu::Synchronize(SynchronizePdu { target_user_id: USER_CHANNEL }),
        ShareDataPdu::Control(ControlPdu { action: ControlAction::Cooperate, grant_id: 0, control_id: 1002 }),
        ShareDataPdu::Control(ControlPdu { action: ControlAction::GrantedControl, grant_id: 0, control_id: 1002 }),
        ShareDataPdu::FontMap(FontPdu::default()),
    ]
}

/// The skipped updates were xrdp's repaint, so the client asks for the whole screen again.
#[test]
fn skipped_graphics_are_followed_by_a_full_refresh_request() {
    use ironrdp_core::decode;
    use ironrdp_pdu::geometry::InclusiveRectangle;
    use ironrdp_pdu::mcs::SendDataRequest;

    let mut seq = ConnectionFinalizationSequence::new(IO_CHANNEL, USER_CHANNEL, 0x0001_03EA)
        .with_repaint_area(Some((1000, 700)));
    let mut replies = vec![ShareDataPdu::Update(vec![1, 0, 4, 0])];
    replies.extend(normal_replies());
    let sent = finalize_with(&mut seq, replies);
    assert!(!sent.is_empty(), "a Refresh Rect PDU is sent after the Font Map");

    let request: X224<SendDataRequest<'_>> = decode(&sent).expect("MCS send data request");
    let header: ShareControlHeader = decode(&request.0.user_data).expect("share control header");
    let ShareControlPdu::Data(data) = header.share_control_pdu else { panic!("not a data PDU") };
    let ShareDataPdu::RefreshRectangle(refresh) = data.share_data_pdu else { panic!("not a Refresh Rect PDU") };
    assert_eq!(refresh.areas_to_refresh, vec![InclusiveRectangle { left: 0, top: 0, right: 999, bottom: 699 }]);
}

#[test]
fn no_refresh_request_when_nothing_was_skipped() {
    let mut seq = ConnectionFinalizationSequence::new(IO_CHANNEL, USER_CHANNEL, 0x0001_03EA)
        .with_repaint_area(Some((1000, 700)));
    assert!(finalize_with(&mut seq, normal_replies()).is_empty());
}

#[test]
fn no_refresh_request_when_server_lacks_refresh_rect_support() {
    let mut seq = ConnectionFinalizationSequence::new(IO_CHANNEL, USER_CHANNEL, 0x0001_03EA).with_repaint_area(None);
    let mut replies = vec![ShareDataPdu::Update(vec![1, 0, 4, 0])];
    replies.extend(normal_replies());
    assert!(finalize_with(&mut seq, replies).is_empty());
}

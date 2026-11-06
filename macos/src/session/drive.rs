//! A Mac folder shared with the remote computer as a drive (RDPDR).
//! Windows shows it as "<name> on Mac" (our RDP client name is "Mac").

use std::path::Path;

use ironrdp_rdpdr::{Rdpdr, RdpdrBackendFactory};
use ironrdp_rdpdr_native::backend::NixRdpdrBackendFactory;

const DRIVE_DEVICE_ID: u32 = 1;
/// The computer name Windows shows after a redirected drive ("Downloads on Mac").
const RDPDR_COMPUTER_NAME: &str = "Mac";

pub fn drive_name(folder: &Path) -> String {
    folder
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "Mac".to_owned())
}

/// The RDPDR channel sharing `folder`, announced to the server as computer "Mac".
/// (ironrdp-client's own RDPDR channel always calls itself "IronRDP".)
pub fn channel(folder: &Path) -> anyhow::Result<Rdpdr> {
    let product = factory(folder)
        .build_rdpdr_backend()
        .map_err(|e| anyhow::anyhow!("shared folder {}: {e}", folder.display()))?;
    let (backend, drives) = product.into_parts();
    let drives = drives.into_iter().map(|d| d.into_parts()).collect();
    Ok(Rdpdr::new(backend, RDPDR_COMPUTER_NAME.to_owned()).with_drives(Some(drives)))
}

/// RDPDR backend serving `folder` (paths from the server stay inside it).
pub fn factory(folder: &Path) -> Box<dyn RdpdrBackendFactory + Send> {
    Box::new(
        NixRdpdrBackendFactory::new(folder.to_string_lossy().into_owned())
            .with_drive(DRIVE_DEVICE_ID, drive_name(folder)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn drive_name_uses_folder_name() {
        assert_eq!(drive_name(Path::new("/Users/me/Downloads")), "Downloads");
        assert_eq!(drive_name(Path::new("/Users/me/My Files/")), "My Files");
    }

    #[test]
    fn drive_name_root_is_mac() {
        assert_eq!(drive_name(Path::new("/")), "Mac");
    }

    #[test]
    fn channel_builds_for_existing_folder() {
        let dir = tempfile::tempdir().unwrap();
        assert!(channel(dir.path()).is_ok());
    }

    #[test]
    fn factory_builds_product_with_one_drive() {
        let dir = tempfile::tempdir().unwrap();
        assert!(factory(dir.path()).build_rdpdr_backend().is_ok());
    }
}

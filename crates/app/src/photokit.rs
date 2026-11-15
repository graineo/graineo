//! Apple Photos via PhotoKit (Apple's API): request permission and have the original of a photo
//! written to a file – downloaded from iCloud if necessary. The Photos library itself is never touched.
//! All calls wait for Apple's response and therefore belong in a background thread.

use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool};
use objc2::{class, msg_send};
use objc2_foundation::{NSArray, NSError, NSString};

#[link(name = "Photos", kind = "framework")]
extern "C" {}

/// PHAccessLevelReadWrite
const READ_WRITE: isize = 2;
/// PHAuthorizationStatus: 0 not determined, 1 restricted (parental controls), 2 denied, 3 authorized, 4 limited
const AUTHORIZED: isize = 3;
const LIMITED: isize = 4;
const PENDING: isize = 0;

fn status() -> isize {
    unsafe { msg_send![class!(PHPhotoLibrary), authorizationStatusForAccessLevel: READ_WRITE] }
}

/// Ensure permission; the first time macOS shows its dialog.
pub fn permission() -> Result<(), String> {
    let mut s = status();
    if s == PENDING {
        let (tx, rx) = mpsc::channel::<isize>();
        let block = RcBlock::new(move |st: isize| {
            let _ = tx.send(st);
        });
        unsafe {
            let _: () = msg_send![class!(PHPhotoLibrary), requestAuthorizationForAccessLevel: READ_WRITE, handler: &*block];
        }
        s = rx.recv_timeout(Duration::from_secs(600)).map_err(|_| crate::tr("Keine Antwort auf die Frage nach dem Zugriff auf Fotos").to_string())?;
    }
    match s {
        AUTHORIZED | LIMITED => Ok(()),
        _ => Err(crate::tr("Kein Zugriff auf Fotos – bitte unter Systemeinstellungen → Datenschutz & Sicherheit → Fotos für graineo erlauben").to_string()),
    }
}

/// The photo with this identifier (`ZUUID`) as a PhotoKit object.
fn asset(uuid: &str) -> Result<Retained<AnyObject>, String> {
    let id = NSString::from_str(&format!("{uuid}/L0/001"));
    let ids = NSArray::from_retained_slice(&[id]);
    unsafe {
        let fetched: Retained<AnyObject> = msg_send![class!(PHAsset), fetchAssetsWithLocalIdentifiers: &*ids, options: std::ptr::null::<AnyObject>()];
        let a: Option<Retained<AnyObject>> = msg_send![&*fetched, firstObject];
        a.ok_or_else(|| crate::tr("Foto in Apple Fotos nicht gefunden").to_string())
    }
}

/// Write the original (unedited, as captured) to `target`; downloads it from iCloud if needed.
pub fn write_original(uuid: &str, target: &Path) -> Result<(), String> {
    permission()?;
    let a = asset(uuid)?;
    unsafe {
        let resources: Retained<NSArray<AnyObject>> = msg_send![class!(PHAssetResource), assetResourcesForAsset: &*a];
        // PHAssetResourceType: 1 photo, 2 video, 4 alternate photo (RAW+JPEG), 5 edited version
        let mut chosen: Option<Retained<AnyObject>> = None;
        for q in resources.iter() {
            let res_type: isize = msg_send![&*q, type];
            if res_type == 1 || (chosen.is_none() && res_type == 2) {
                chosen = Some(q.clone());
                if res_type == 1 {
                    break;
                }
            }
        }
        let resource = chosen.ok_or_else(|| crate::tr("Kein Original in Apple Fotos").to_string())?;
        let options: Retained<AnyObject> = msg_send![class!(PHAssetResourceRequestOptions), new];
        let _: () = msg_send![&*options, setNetworkAccessAllowed: Bool::YES];
        let path = NSString::from_str(&target.to_string_lossy());
        let url: Retained<AnyObject> = msg_send![class!(NSURL), fileURLWithPath: &*path];
        let manager: Retained<AnyObject> = msg_send![class!(PHAssetResourceManager), defaultManager];
        let (tx, rx) = mpsc::channel::<Result<(), String>>();
        let block = RcBlock::new(move |error: *mut NSError| {
            let r = if error.is_null() {
                Ok(())
            } else {
                let text: Retained<NSString> = msg_send![&*error, localizedDescription];
                Err(text.to_string())
            };
            let _ = tx.send(r);
        });
        let _: () = msg_send![&*manager, writeDataForAssetResource: &*resource, toFile: &*url, options: &*options, completionHandler: &*block];
        rx.recv_timeout(Duration::from_secs(900)).map_err(|_| crate::tr("Laden aus iCloud dauert zu lange").to_string())?
    }
}

/// Result of [`delete`].
#[derive(Debug, PartialEq)]
pub enum Deleted {
    /// this many photos are now in Apple Photos under "Recently Deleted" (identifiers of those found)
    Ja(Vec<String>),
    /// declined in the Apple Photos dialog
    Cancelled,
}

/// Delete photos via Apple Photos. macOS asks once more itself; the photos end up in
/// "Recently Deleted" (recoverable for 30 days) and disappear via iCloud on the other devices too.
pub fn delete(uuids: &[String]) -> Result<Deleted, String> {
    if uuids.is_empty() {
        return Ok(Deleted::Ja(Vec::new()));
    }
    permission()?;
    let ids: Vec<Retained<NSString>> = uuids.iter().map(|u| NSString::from_str(&format!("{u}/L0/001"))).collect();
    let ids = NSArray::from_retained_slice(&ids);
    unsafe {
        let fetched: Retained<AnyObject> = msg_send![class!(PHAsset), fetchAssetsWithLocalIdentifiers: &*ids, options: std::ptr::null::<AnyObject>()];
        let count: usize = msg_send![&*fetched, count];
        if count == 0 {
            return Err(crate::tr("Fotos in Apple Fotos nicht gefunden").to_string());
        }
        let mut found = Vec::with_capacity(count);
        for i in 0..count {
            let a: Retained<AnyObject> = msg_send![&*fetched, objectAtIndex: i];
            let identifier: Retained<NSString> = msg_send![&*a, localIdentifier];
            found.push(identifier.to_string().split('/').next().unwrap_or_default().to_string());
        }
        let list = fetched.clone();
        let change = RcBlock::new(move || {
            let _: () = msg_send![class!(PHAssetChangeRequest), deleteAssets: &*list];
        });
        let (tx, rx) = mpsc::channel::<Result<bool, (isize, String)>>();
        let completion = RcBlock::new(move |ok: Bool, error: *mut NSError| {
            let r = if ok.as_bool() {
                Ok(true)
            } else if error.is_null() {
                Ok(false)
            } else {
                let code: isize = msg_send![&*error, code];
                let text: Retained<NSString> = msg_send![&*error, localizedDescription];
                Err((code, text.to_string()))
            };
            let _ = tx.send(r);
        });
        let library: Retained<AnyObject> = msg_send![class!(PHPhotoLibrary), sharedPhotoLibrary];
        let _: () = msg_send![&*library, performChanges: &*change, completionHandler: &*completion];
        // Apple's confirmation waits for the user
        match rx.recv_timeout(Duration::from_secs(3600)) {
            Ok(Ok(true)) => Ok(Deleted::Ja(found)),
            // PHPhotosErrorUserCancelled (3072) or declined without an error
            Ok(Ok(false)) | Ok(Err((3072, _))) => Ok(Deleted::Cancelled),
            Ok(Err((_, text))) => Err(text),
            Err(_) => Err(crate::tr("Keine Antwort von Apple Fotos").to_string()),
        }
    }
}

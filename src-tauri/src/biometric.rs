//! A biometric check (Face ID, Touch ID, Android fingerprint / face) before a password
//! remembered on this device is used (`vault.rs`).
//!
//! macOS and iOS ask LocalAuthentication directly; Android goes through
//! `ViewPortPlugin.kt` (androidx BiometricPrompt). Other platforms have no biometrics.
//! Every call blocks until the user answers, so run it off the async runtime.

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Status {
    /// Biometrics are enrolled and usable now.
    pub available: bool,
    /// The platform can have biometrics at all (false on Windows and Linux).
    pub supported: bool,
    /// "face", "touch", "optic", "fingerprint" or "" when unknown.
    pub kind: String,
}

/// How a biometric check ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Passed,
    /// The user cancelled (or chose the fallback).
    Cancelled,
    /// The check could not run or did not recognise the user.
    Failed,
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
mod imp {
    use std::sync::mpsc;

    use block2::RcBlock;
    use objc2::runtime::Bool;
    use objc2_foundation::{NSError, NSString};
    use objc2_local_authentication::{LABiometryType, LAContext, LAError, LAPolicy};

    use super::{Outcome, Status};

    const POLICY: LAPolicy = LAPolicy::DeviceOwnerAuthenticationWithBiometrics;

    pub fn status() -> Status {
        unsafe {
            let ctx = LAContext::new();
            let available = ctx.canEvaluatePolicy_error(POLICY).is_ok();
            let kind = match ctx.biometryType() {
                LABiometryType::FaceID => "face",
                LABiometryType::TouchID => "touch",
                LABiometryType::OpticID => "optic",
                _ => "",
            };
            Status { available, supported: true, kind: kind.into() }
        }
    }

    pub fn authenticate(reason: &str, cancel: &str) -> Outcome {
        let (tx, rx) = mpsc::channel::<Outcome>();
        unsafe {
            let ctx = LAContext::new();
            if ctx.canEvaluatePolicy_error(POLICY).is_err() {
                return Outcome::Failed;
            }
            if !cancel.is_empty() {
                ctx.setLocalizedCancelTitle(Some(&NSString::from_str(cancel)));
            }
            // An empty fallback title hides "Enter password": only biometrics are offered.
            ctx.setLocalizedFallbackTitle(Some(&NSString::from_str("")));
            let reason = NSString::from_str(if reason.is_empty() { "ViewPort" } else { reason });
            let block = RcBlock::new(move |ok: Bool, err: *mut NSError| {
                let outcome = if ok.as_bool() {
                    Outcome::Passed
                } else {
                    let code = err.as_ref().map(|e| e.code()).unwrap_or(0);
                    if code == LAError::UserCancel.0 || code == LAError::UserFallback.0 || code == LAError::SystemCancel.0 || code == LAError::AppCancel.0 {
                        Outcome::Cancelled
                    } else {
                        Outcome::Failed
                    }
                };
                let _ = tx.send(outcome);
            });
            ctx.evaluatePolicy_localizedReason_reply(POLICY, &reason, &block);
            // The context must stay alive until the reply arrives.
            let out = rx.recv().unwrap_or(Outcome::Failed);
            drop(ctx);
            out
        }
    }
}

#[cfg(target_os = "android")]
mod imp {
    use serde::Deserialize;
    use serde_json::json;

    use super::{Outcome, Status};

    #[derive(Deserialize)]
    struct NativeStatus {
        available: bool,
    }

    #[derive(Deserialize)]
    struct NativeResult {
        result: String,
    }

    pub fn status() -> Status {
        let available = crate::platform::android_handle()
            .and_then(|h| h.run_mobile_plugin::<NativeStatus>("biometricStatus", ()).ok())
            .is_some_and(|s| s.available);
        Status { available, supported: true, kind: "fingerprint".into() }
    }

    pub fn authenticate(reason: &str, cancel: &str) -> Outcome {
        let Some(h) = crate::platform::android_handle() else { return Outcome::Failed };
        match h.run_mobile_plugin::<NativeResult>("biometricAuth", json!({ "title": reason, "cancel": cancel })) {
            Ok(r) if r.result == "passed" => Outcome::Passed,
            Ok(r) if r.result == "cancelled" => Outcome::Cancelled,
            _ => Outcome::Failed,
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "android")))]
mod imp {
    use super::{Outcome, Status};

    pub fn status() -> Status {
        Status { available: false, supported: false, kind: String::new() }
    }

    pub fn authenticate(_reason: &str, _cancel: &str) -> Outcome {
        Outcome::Failed
    }
}

pub use imp::{authenticate, status};

#[cfg(test)]
mod tests {
    /// Asking what the device has never shows a prompt.
    #[test]
    fn status_answers_without_prompting() {
        let s = super::status();
        assert_eq!(s.supported, cfg!(any(target_os = "macos", target_os = "ios", target_os = "android")));
        assert!(s.supported || !s.available);
        eprintln!("biometric status: {s:?}");
    }
}

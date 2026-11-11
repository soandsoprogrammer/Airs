use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::sync::{Notify, mpsc};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SignInStatus {
    #[default]
    Loading,
    WaitingForScan,
    ConfirmOnPhone,
    Expired,
    VerifyInApp,
    CheckingAccount,
}

#[derive(Clone, Default, PartialEq, Eq)]
pub struct SignInPreview {
    pub png: Option<Arc<Vec<u8>>>,
    pub status: SignInStatus,
    pub verification: Option<VerificationFrame>,
    pub verification_id: Option<u64>,
    pub input_feedback: Option<&'static str>,
    pub publish_sms: Option<PublishSmsPreview>,
}

impl SignInPreview {
    fn set_status(&mut self, status: SignInStatus) {
        static NEXT_VERIFICATION_ID: AtomicU64 = AtomicU64::new(1);
        self.status = status;
        if status == SignInStatus::VerifyInApp {
            self.verification_id
                .get_or_insert_with(|| NEXT_VERIFICATION_ID.fetch_add(1, Ordering::Relaxed));
        } else {
            self.verification = None;
            self.verification_id = None;
            self.input_feedback = None;
            self.publish_sms = None;
        }
    }

    fn accepts_action(&self, verification_id: u64, action: &VerificationAction) -> bool {
        self.verification_id == Some(verification_id)
            && match action {
                VerificationAction::RequestSmsCode => {
                    self.publish_sms.is_some_and(|sms| sms.can_request_code)
                }
                VerificationAction::VerifySmsCode(_) => {
                    self.publish_sms.is_some_and(|sms| sms.can_verify)
                }
                VerificationAction::Click { .. } | VerificationAction::FillText(_) => {
                    self.verification.is_some()
                }
            }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct VerificationFrame {
    pub png: Arc<Vec<u8>>,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
pub struct PublishSmsPreview {
    pub can_request_code: bool,
    pub can_verify: bool,
    pub resend_seconds: Option<u16>,
}

pub enum VerificationAction {
    Click { x: f64, y: f64 },
    FillText(String),
    RequestSmsCode,
    VerifySmsCode(String),
}

struct SignIn {
    preview: Mutex<SignInPreview>,
    refresh: Notify,
    verification_tx: mpsc::Sender<(u64, VerificationAction)>,
}

/// Shares active sign-in previews with the desktop without persisting them.
#[derive(Clone, Default)]
pub struct SignIns {
    active: Arc<Mutex<HashMap<String, Arc<SignIn>>>>,
}

impl SignIns {
    pub fn begin(&self, account_id: &str) -> SignInGuard {
        let account_id = account_id.to_owned();
        let (verification_tx, verification_rx) = mpsc::channel(32);
        let entry = Arc::new(SignIn {
            preview: Mutex::new(SignInPreview::default()),
            refresh: Notify::new(),
            verification_tx,
        });
        self.active
            .lock()
            .unwrap()
            .insert(account_id.clone(), entry.clone());
        SignInGuard {
            sign_ins: self.clone(),
            account_id,
            entry,
            verification_rx: tokio::sync::Mutex::new(verification_rx),
        }
    }

    pub fn snapshot(&self) -> HashMap<String, SignInPreview> {
        self.active
            .lock()
            .unwrap()
            .iter()
            .map(|(id, entry)| (id.clone(), entry.preview.lock().unwrap().clone()))
            .collect()
    }

    pub fn request_refresh(&self, account_id: &str) -> bool {
        let active = self.active.lock().unwrap();
        let Some(entry) = active.get(account_id) else {
            return false;
        };
        entry.refresh.notify_one();
        true
    }

    pub fn send_verification_action(
        &self,
        account_id: &str,
        verification_id: u64,
        action: VerificationAction,
    ) -> bool {
        let active = self.active.lock().unwrap();
        let Some(entry) = active.get(account_id) else {
            return false;
        };
        let preview = entry.preview.lock().unwrap();
        preview.accepts_action(verification_id, &action)
            && entry
                .verification_tx
                .try_send((verification_id, action))
                .is_ok()
    }
}

/// Keeps the preview visible only while its corresponding sign-in is active.
#[must_use = "dropping the guard removes the sign-in preview"]
pub struct SignInGuard {
    sign_ins: SignIns,
    account_id: String,
    entry: Arc<SignIn>,
    verification_rx: tokio::sync::Mutex<mpsc::Receiver<(u64, VerificationAction)>>,
}

impl SignInGuard {
    pub fn update(&self, status: SignInStatus, png: Option<Vec<u8>>) {
        let mut preview = self.entry.preview.lock().unwrap();
        preview.set_status(status);
        if preview.png.as_deref() != png.as_ref() {
            preview.png = png.map(Arc::new);
        }
    }

    pub async fn refresh_requested(&self) {
        self.entry.refresh.notified().await;
    }

    pub fn set_input_feedback(&self, feedback: Option<&'static str>) {
        let mut preview = self.entry.preview.lock().unwrap();
        if preview.status == SignInStatus::VerifyInApp {
            preview.input_feedback = feedback;
        }
    }

    pub fn update_verification(&self, png: Vec<u8>, width: u32, height: u32) {
        let mut preview = self.entry.preview.lock().unwrap();
        preview.set_status(SignInStatus::VerifyInApp);
        preview.png = None;
        preview.publish_sms = None;
        if !preview.verification.as_ref().is_some_and(|frame| {
            frame.width == width && frame.height == height && frame.png.as_ref() == &png
        }) {
            preview.verification = Some(VerificationFrame {
                png: Arc::new(png),
                width,
                height,
            });
        }
    }

    pub(crate) fn update_publish_sms(&self, sms: PublishSmsPreview) {
        let mut preview = self.entry.preview.lock().unwrap();
        preview.set_status(SignInStatus::VerifyInApp);
        preview.png = None;
        preview.verification = None;
        preview.publish_sms = Some(sms);
    }

    pub async fn verification_action(&self) -> Option<VerificationAction> {
        let mut actions = self.verification_rx.lock().await;
        while let Some((verification_id, action)) = actions.recv().await {
            if self
                .entry
                .preview
                .lock()
                .unwrap()
                .accepts_action(verification_id, &action)
            {
                return Some(action);
            }
        }
        None
    }
}

impl Drop for SignInGuard {
    fn drop(&mut self) {
        let mut active = self.sign_ins.active.lock().unwrap();
        if active
            .get(&self.account_id)
            .is_some_and(|entry| Arc::ptr_eq(entry, &self.entry))
        {
            active.remove(&self.account_id);
        }
    }
}

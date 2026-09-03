#![no_main]

use libfuzzer_sys::fuzz_target;
use w9pt::{
    Completion, Effect, FilesystemError, LinuxErrno, PolicyError, Session, SessionConfig,
    SessionContext, SessionId,
};

fuzz_target!(|data: &[u8]| {
    let Ok(mut session) = Session::new(
        SessionConfig::default(),
        SessionContext::new(SessionId::new(1)),
    ) else {
        return;
    };

    for action in data.chunks(16) {
        if action.first().is_some_and(|byte| byte & 1 == 0) {
            let _ = session.receive_bytes(&action[1..]);
        }
        while let Some(effect) = session.poll_effect() {
            match effect {
                Effect::Filesystem { operation_id, .. } => {
                    let _ = session.complete(Completion::Filesystem {
                        operation_id,
                        result: Err(FilesystemError::new(LinuxErrno::EIO)),
                    });
                }
                Effect::Policy { operation_id, .. } => {
                    let _ = session.complete(Completion::Policy {
                        operation_id,
                        result: Err(PolicyError::new(LinuxErrno::EACCES)),
                    });
                }
                Effect::SendFrame { .. }
                | Effect::Cancel { .. }
                | Effect::CloseSession { .. } => {}
            }
        }
    }
    session.begin_close();
    while session.poll_effect().is_some() {}
});

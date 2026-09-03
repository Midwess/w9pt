use w9pt::{
    Completion, CompletionError, Effect, Session, SessionConfig, SessionContext, SessionError,
    SessionId,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModelEvent {
    Input(Vec<u8>),
    Effect(Effect),
    Completion(Completion),
}

pub struct ModelDriver {
    pub session: Session,
    pub trace: Vec<ModelEvent>,
}

impl ModelDriver {
    pub fn new(session_id: u64) -> Self {
        Self {
            session: Session::new(
                SessionConfig::default(),
                SessionContext::new(SessionId::new(session_id)),
            )
            .unwrap(),
            trace: Vec::new(),
        }
    }

    pub fn input(&mut self, frame: Vec<u8>) -> Result<(), SessionError> {
        self.trace.push(ModelEvent::Input(frame.clone()));
        self.session.receive_frame(frame)
    }

    pub fn effect(&mut self) -> Option<Effect> {
        let effect = self.session.poll_effect()?;
        self.trace.push(ModelEvent::Effect(effect.clone()));
        Some(effect)
    }

    pub fn complete(&mut self, completion: Completion) -> Result<(), CompletionError> {
        self.trace.push(ModelEvent::Completion(completion.clone()));
        self.session.complete(completion)
    }
}

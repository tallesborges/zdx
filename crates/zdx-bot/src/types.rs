use std::path::PathBuf;

pub struct IncomingMessage {
    pub chat_id: i64,
    pub message_id: i64,
    pub user_id: i64,
    pub text: Option<String>,
    pub images: Vec<IncomingImage>,
    pub audios: Vec<IncomingAudio>,
}

pub struct IncomingImage {
    pub local_path: PathBuf,
    pub mime_type: String,
    pub data: String,
}

pub struct IncomingAudio {
    pub local_path: PathBuf,
    pub transcript: Option<String>,
}

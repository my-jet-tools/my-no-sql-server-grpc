use std::sync::Arc;

use crate::app::AppContext;

pub struct WriterGrpcService {
    pub app: Arc<AppContext>,
}

impl WriterGrpcService {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

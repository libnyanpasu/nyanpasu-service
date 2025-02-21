use std::{
    borrow::Cow,
    sync::{Arc, OnceLock},
};

use bounded_vec_deque::BoundedVecDeque;
use indexmap::IndexMap;
use parking_lot::Mutex;
use tracing_subscriber::fmt::MakeWriter;

pub type LoggerSubscriber = Box<dyn Fn(TracingLogging) + Send + Sync + 'static>;

pub struct Logger<'n> {
    buffer: Arc<Mutex<BoundedVecDeque<Cow<'n, str>>>>,
    subscriber: Arc<OnceLock<LoggerSubscriber>>,
}

#[derive(Debug, serde::Deserialize)]
pub struct TracingLogging {
    pub level: String,
    pub timestamp: String,
    #[serde(flatten)]
    pub fields: IndexMap<String, serde_json::Value>,
}

impl Clone for Logger<'_> {
    fn clone(&self) -> Self {
        Logger {
            buffer: self.buffer.clone(),
            subscriber: self.subscriber.clone(),
        }
    }
}

impl<'n> Logger<'n> {
    pub fn global() -> &'static Logger<'static> {
        static INSTANCE: OnceLock<Logger> = OnceLock::new();
        INSTANCE.get_or_init(|| Logger {
            buffer: Arc::new(Mutex::new(BoundedVecDeque::new(100))),
            subscriber: Arc::new(OnceLock::new()),
        })
    }

    pub fn set_subscriber(
        &self,
        subscriber: Box<dyn Fn(TracingLogging) + Send + Sync + 'static>,
    ) -> bool {
        self.subscriber.set(subscriber).is_ok()
    }

    /// Retrieve all logs in the buffer
    /// It should clear the buffer after retrieve
    pub fn retrieve_logs(&self) -> Vec<Cow<'n, str>> {
        let mut buffer = self.buffer.lock();
        buffer.drain(..).collect()
    }

    /// Inspect all logs in the buffer
    /// It should not clear the buffer after inspect
    pub fn inspect_logs(&self) -> Vec<Cow<'n, str>> {
        let buffer = self.buffer.lock();
        buffer.iter().cloned().collect()
    }
}

impl std::io::Write for Logger<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut buffer = self.buffer.lock();
        let msg = String::from_utf8_lossy(buf);
        if let Some(subscriber) = self.subscriber.get() {
            if let Ok(logging) = serde_json::from_str::<TracingLogging>(&msg) {
                subscriber(logging);
            }
        }
        buffer.push_back(Cow::Owned(msg.into_owned()));
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Logger<'static> {
    type Writer = Logger<'static>;

    fn make_writer(&'a self) -> Self::Writer {
        Logger {
            buffer: self.buffer.clone(),
            subscriber: self.subscriber.clone(),
        }
    }
}

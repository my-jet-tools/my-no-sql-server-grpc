use std::time::Duration;

use tonic::transport::{Channel, Endpoint};

use crate::MyNoSqlWriterError;
use crate::my_no_sql_writer_grpc::writer_client::WriterClient;

/// How long a **unary** call may take before it is given up on. One of those is
/// one round trip against an in-memory database, so a call still going after
/// this is not slow, it is lost.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// One connection to the server, shared by as many writers as there are entity
/// types.
///
/// It is built lazily and never has to be rebuilt: tonic's channel reconnects
/// underneath, so a server which went away and came back is picked up without
/// anybody here noticing. Cloning is cheap - the channel is a handle.
#[derive(Clone)]
pub struct MyNoSqlGrpcConnection {
    channel: Channel,
    timeout: Duration,
}

impl MyNoSqlGrpcConnection {
    pub fn new(url: impl Into<String>) -> Result<Self, MyNoSqlWriterError> {
        Self::with_timeout(url, DEFAULT_TIMEOUT)
    }

    /// The deadline is **not** put on the channel.
    ///
    /// `Endpoint::timeout` races the *response* future, and this crate drives
    /// three client-streaming calls - `BulkWrite`, `UploadBackup` and posting a
    /// transaction - where the server sends nothing until it has consumed the
    /// whole request stream. A channel-wide deadline therefore covers the entire
    /// upload, so a batch big enough to spend thirty seconds on the wire can
    /// never succeed, at any number of retries. Server-streaming reads would
    /// survive it - they resolve on the response headers - but that is not a
    /// distinction worth relying on a channel to make.
    ///
    /// So the deadline is put on the requests it means something for, by
    /// [`Self::unary`].
    pub fn with_timeout(
        url: impl Into<String>,
        timeout: Duration,
    ) -> Result<Self, MyNoSqlWriterError> {
        let url = url.into();

        let Ok(endpoint) = Endpoint::from_shared(url.clone()) else {
            return Err(MyNoSqlWriterError::InvalidUrl(url));
        };

        Ok(Self {
            channel: endpoint.connect_lazy(),
            timeout,
        })
    }

    /// A request with the deadline on it. Used by the calls which are one round
    /// trip and by nothing else: a stream is as long as its data, and a deadline
    /// on one of those is a size limit written in seconds.
    pub(crate) fn unary<TMessage>(&self, message: TMessage) -> tonic::Request<TMessage> {
        let mut request = tonic::Request::new(message);
        request.set_timeout(self.timeout);
        request
    }

    pub(crate) fn writer_client(&self) -> WriterClient<Channel> {
        WriterClient::new(self.channel.clone())
    }
}

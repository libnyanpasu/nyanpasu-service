use futures_util::stream::Stream;
use http_body_util::BodyDataStream;
use hyper::body::Body;
use pin_project_lite::pin_project;
use std::task::Poll;

pin_project! {
    #[derive(Clone, Copy, Debug)]
    pub(super) struct ResponseBodyStreamWrapper<S>
    {
        #[pin]
        inner: S,
    }
}

impl<B> Stream for ResponseBodyStreamWrapper<BodyDataStream<B>>
where
    B: Body,
    B::Error: Into<anyhow::Error>,
{
    type Item = Result<B::Data, std::io::Error>;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let mut this = self.project();
        match futures_util::StreamExt::poll_next_unpin(&mut this.inner, cx) {
            Poll::Ready(Some(Ok(bytes))) => Poll::Ready(Some(Ok(bytes))),
            Poll::Ready(Some(Err(e))) => {
                let io_err = std::io::Error::other(e.into());
                Poll::Ready(Some(Err(io_err)))
            }
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

pub(super) trait BodyDataStreamExt {
    fn into_stream_wrapper(self) -> ResponseBodyStreamWrapper<Self>
    where
        Self: Sized;
}

impl<S> BodyDataStreamExt for BodyDataStream<S>
where
    S: Body,
{
    fn into_stream_wrapper(self) -> ResponseBodyStreamWrapper<BodyDataStream<S>> {
        ResponseBodyStreamWrapper { inner: self }
    }
}

use actix_web::{
    body::MessageBody,
    dev::{Service, ServiceRequest, ServiceResponse, Transform},
    Error,
};
use metrics::counter;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::{
    future::{ready, Future, Ready},
    mem::replace,
    time::Instant,
};

/// Creates metrics based on request.
pub struct Metrics;

impl<S, B> Transform<S, ServiceRequest> for Metrics
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error>,
    S::Future: 'static,
    B: MessageBody + 'static,
{
    type Response = ServiceResponse<B>;
    type Error = Error;
    type Transform = MetricsMiddleware<S>;
    type InitError = ();
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        ready(Ok(MetricsMiddleware { service }))
    }
}

#[doc(hidden)]
pub struct MetricsMiddleware<S> {
    service: S,
}

impl<B, S> Service<ServiceRequest> for MetricsMiddleware<S>
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error>,
    S::Future: 'static,
    B: MessageBody + 'static,
{
    type Error = Error;
    type Future = MetricsResponse<S::Future>;
    type Response = ServiceResponse<B>;

    actix_web::dev::forward_ready!(service);

    fn call(&self, req: ServiceRequest) -> Self::Future {
        let route = req.path().to_string();
        let f = self.service.call(req);

        MetricsResponse {
            f,
            state: ResponseStateGuard(ResponseState::NotStarted { route }),
        }
    }
}

enum ResponseState {
    NotStarted { route: String },
    Started { instant: Instant, route: String },
    Completed,
}

impl ResponseState {
    fn complete(&mut self) {
        *self = match replace(self, Self::Completed) {
            Self::Started { instant, route } => {
                counter!("req_time_ms", "route" => route.clone())
                    .increment(instant.elapsed().as_millis() as u64);

                counter!("req_count", "route" => route.clone()).increment(1);

                Self::Completed
            }
            state => state,
        };
    }

    fn start(&mut self) {
        *self = match replace(self, Self::Completed) {
            Self::NotStarted { route } => {
                counter!("req_started", "route" => route.clone()).increment(1);

                Self::Started {
                    instant: Instant::now(),
                    route,
                }
            }
            state => state,
        };
    }
}

struct ResponseStateGuard(ResponseState);

impl Drop for ResponseStateGuard {
    fn drop(&mut self) {
        self.0.complete();
    }
}

#[doc(hidden)]
#[pin_project::pin_project]
pub struct MetricsResponse<F> {
    #[pin]
    f: F,
    state: ResponseStateGuard,
}

impl<F, B> Future for MetricsResponse<F>
where
    B: MessageBody + 'static,
    F: Future<Output = Result<ServiceResponse<B>, Error>>,
{
    type Output = Result<ServiceResponse<B>, Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();

        this.state.0.start();

        match this.f.poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(v) => {
                this.state.0.complete();

                Poll::Ready(v)
            }
        }
    }
}

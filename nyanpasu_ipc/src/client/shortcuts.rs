use std::{borrow::Cow, sync::OnceLock};

use axum::body::Body;
use hyper::{header::CONTENT_TYPE, Request};

use crate::{api, SERVICE_PLACEHOLDER};

use super::{send_request, ClientError};

pub struct Client<'a>(Cow<'a, str>);

impl<'a> Client<'a> {
    pub fn new(placeholder: &'a str) -> Self {
        Self(Cow::Borrowed(placeholder))
    }

    pub fn service_default() -> &'static Client<'static> {
        static CLIENT: OnceLock<Client<'static>> = OnceLock::new();
        CLIENT.get_or_init(|| Client::new(SERVICE_PLACEHOLDER))
    }

    pub async fn status(&self) -> Result<api::status::StatusResBody<'_>, ClientError> {
        let request = Request::get(api::status::STATUS_ENDPOINT).body(Body::empty())?;
        let response = send_request(&self.0, request)
            .await?
            .cast_body::<api::status::StatusRes<'_>>()
            .await?
            .ok()?;
        let data = response.data.unwrap();
        Ok(data)
    }

    pub async fn start_core(
        &self,
        payload: &api::core::start::CoreStartReq,
    ) -> Result<(), ClientError> {
        let payload = simd_json::serde::to_string(payload)?;
        let request = Request::post(api::core::start::CORE_START_ENDPOINT)
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(payload))?;
        let response = send_request(&self.0, request)
            .await?
            .cast_body::<api::core::start::CoreStartRes>()
            .await?;
        response.ok()?;
        Ok(())
    }

    pub async fn stop_core(&self) -> Result<(), ClientError> {
        let request = Request::post(api::core::stop::CORE_STOP_ENDPOINT).body(Body::empty())?;
        let response = send_request(&self.0, request)
            .await?
            .cast_body::<api::core::stop::CoreStopRes>()
            .await?;
        response.ok()?;
        Ok(())
    }

    pub async fn restart_core(&self) -> Result<(), ClientError> {
        let request =
            Request::post(api::core::restart::CORE_RESTART_ENDPOINT).body(Body::empty())?;
        let response = send_request(&self.0, request)
            .await?
            .cast_body::<api::core::restart::CoreRestartRes>()
            .await?;
        response.ok()?;
        Ok(())
    }
}

use std::borrow::Cow;

use axum::body::Body;
use hyper::Request;

use crate::api;

use super::{send_request, ClientError};

pub struct Client<'a>(Cow<'a, str>);

impl<'a> Client<'a> {
    pub fn new(placeholder: &'a str) -> Self {
        Self(Cow::Borrowed(placeholder))
    }

    pub async fn status(&self) -> Result<api::status::StatusResBody<'_>, ClientError> {
        let request = Request::get(api::status::STATUS_ENDPOINT).body(Body::empty())?;
        let response = send_request(&self.0, request)
            .await?
            .cast_body::<api::status::StatusRes<'_>>()
            .await?;
        if response.code != api::ResponseCode::Ok {
            return Err(ClientError::Other(anyhow::anyhow!(
                "Received an error response: {:#?}",
                response
            )));
        }
        let data = response.data.unwrap();
        Ok(data)
    }
}

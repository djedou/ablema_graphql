use std::{error::Error, fmt, string::FromUtf8Error};
use hyper::{
    header::{self, HeaderValue},
    Body, Method, Request, Response, StatusCode,
};
use async_graphql::{
    Schema, BatchRequest, ObjectType, SubscriptionType,
    Request as AsgRequest, context::Variables, Value
};
use serde_json::error::Error as SerdeError;
use url::form_urlencoded;



pub async fn playground(
    graphql_endpoint: &str
) -> Result<Response<Body>, hyper::Error> {
    let mut resp = new_html_response(StatusCode::OK);
    *resp.body_mut() = Body::from(async_graphql::http::playground_source(
        async_graphql::http::GraphQLPlaygroundConfig::new(graphql_endpoint)
    ));
    Ok(resp)
}

pub async fn graphql_sync<QueryT, MutationT, SubscriptionT>(
    schema: Schema<QueryT, MutationT, SubscriptionT>,
    req: Request<Body>,
) -> Result<Response<Body>, hyper::Error>
where
    QueryT: ObjectType + 'static,
    MutationT: ObjectType + 'static,
    SubscriptionT: SubscriptionType + 'static,
{
    Ok(match parse_req(req).await {
        Ok(req) => execute_request_sync(schema, req).await,
        Err(resp) => resp,
    })
}

async fn parse_req(
    req: Request<Body>,
) -> Result<BatchRequest, Response<Body>> {
    match *req.method() {
        Method::GET => parse_get_req(req),
        Method::POST => {
            let content_type = req
                .headers()
                .get(header::CONTENT_TYPE)
                .map(HeaderValue::to_str);
            match content_type {
                Some(Ok("application/json")) => parse_post_json_req(req.into_body()).await,
                Some(Ok("application/graphql")) => parse_post_graphql_req(req.into_body()).await,
                _ => return Err(new_response(StatusCode::BAD_REQUEST)),
            }
        }
        _ => return Err(new_response(StatusCode::METHOD_NOT_ALLOWED)),
    }
    .map_err(render_error)
}

fn parse_get_req(
    req: Request<Body>,
) -> Result<BatchRequest, GraphQLRequestError> {
    req.uri()
        .query()
        .map(|q| gql_request_from_get(q).map(BatchRequest::Single))
        .unwrap_or_else(|| {
            Err(GraphQLRequestError::Invalid(
                "'query' parameter is missing".to_string(),
            ))
        })
}

async fn parse_post_json_req(
    body: Body,
) -> Result<BatchRequest, GraphQLRequestError> {
    let chunk = hyper::body::to_bytes(body)
        .await
        .map_err(GraphQLRequestError::BodyHyper)?;

    let input = String::from_utf8(chunk.iter().cloned().collect())
        .map_err(GraphQLRequestError::BodyUtf8)?;

    serde_json::from_str::<BatchRequest>(&input)
        .map_err(GraphQLRequestError::BodyJSONError)
}

async fn parse_post_graphql_req(
    body: Body,
) -> Result<BatchRequest, GraphQLRequestError> {
    let chunk = hyper::body::to_bytes(body)
        .await
        .map_err(GraphQLRequestError::BodyHyper)?;

    let query = String::from_utf8(chunk.iter().cloned().collect())
        .map_err(GraphQLRequestError::BodyUtf8)?;

    Ok(BatchRequest::Single(AsgRequest::new(
        query
    )))
}

fn render_error(err: GraphQLRequestError) -> Response<Body> {
    let message = format!("{}", err);
    let mut resp = new_response(StatusCode::BAD_REQUEST);
    *resp.body_mut() = Body::from(message);
    resp
}

async fn execute_request_sync<QueryT, MutationT, SubscriptionT>(
    schema: Schema<QueryT, MutationT, SubscriptionT>,
    request: BatchRequest,
) -> Response<Body>
where
    QueryT: ObjectType + 'static,
    MutationT: ObjectType + 'static,
    SubscriptionT: SubscriptionType + 'static,
{
    let res = schema.execute_batch(request).await;
    let body = Body::from(serde_json::to_string_pretty(&res).unwrap());
    let code = if res.is_ok() {
        StatusCode::OK
    } else {
        StatusCode::BAD_REQUEST
    };
    let mut resp = new_response(code);
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    *resp.body_mut() = body;
    resp
}

fn gql_request_from_get(input: &str) -> Result<AsgRequest, GraphQLRequestError> {
    let mut query = None;
    let operation_name: Option<String> = None;
    let mut variables = None;
    for (key, value) in form_urlencoded::parse(input.as_bytes()).into_owned() {
        match key.as_ref() {
            "query" => {
                if query.is_some() {
                    return Err(invalid_err("query"));
                }
                query = Some(value)
            }
            "operationName" => {
                if operation_name.is_some() {
                    return Err(invalid_err("operationName"));
                }
            }
            "variables" => {
                if variables.is_some() {
                    return Err(invalid_err("variables"));
                }
                match serde_json::from_str::<Value>(&value)
                    .map_err(GraphQLRequestError::Variables)
                {
                    Ok(parsed_variables) => variables = Some(parsed_variables),
                    Err(e) => return Err(e),
                }
            }
            _ => continue,
        }
    }
    match query {
        Some(query) => {
            let new_request = AsgRequest::new(query);
            let new_request = new_request.operation_name(operation_name.unwrap_or_default());
            let new_request = new_request.variables(Variables::from_value(variables.unwrap_or_default()));
            //Ok(BatchRequest::Single(new_request))
            Ok(new_request)
        },
        None => Err(GraphQLRequestError::Invalid(
            "'query' parameter is missing".to_string(),
        )),
    }
}

fn invalid_err(parameter_name: &str) -> GraphQLRequestError {
    GraphQLRequestError::Invalid(format!(
        "'{}' parameter is specified multiple times",
        parameter_name
    ))
}

fn new_response(code: StatusCode) -> Response<Body> {
    let mut r = Response::new(Body::empty());
    *r.status_mut() = code;
    r
}

fn new_html_response(code: StatusCode) -> Response<Body> {
    let mut resp = new_response(code);
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    resp
}

#[derive(Debug)]
enum GraphQLRequestError {
    BodyHyper(hyper::Error),
    BodyUtf8(FromUtf8Error),
    BodyJSONError(SerdeError),
    Variables(SerdeError),
    Invalid(String),
}

impl fmt::Display for GraphQLRequestError {
    fn fmt(&self, mut f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            GraphQLRequestError::BodyHyper(ref err) => fmt::Display::fmt(err, &mut f),
            GraphQLRequestError::BodyUtf8(ref err) => fmt::Display::fmt(err, &mut f),
            GraphQLRequestError::BodyJSONError(ref err) => fmt::Display::fmt(err, &mut f),
            GraphQLRequestError::Variables(ref err) => fmt::Display::fmt(err, &mut f),
            GraphQLRequestError::Invalid(ref err) => fmt::Display::fmt(err, &mut f),
        }
    }
}

impl Error for GraphQLRequestError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match *self {
            GraphQLRequestError::BodyHyper(ref err) => Some(err),
            GraphQLRequestError::BodyUtf8(ref err) => Some(err),
            GraphQLRequestError::BodyJSONError(ref err) => Some(err),
            GraphQLRequestError::Variables(ref err) => Some(err),
            GraphQLRequestError::Invalid(_) => None,
        }
    }
}

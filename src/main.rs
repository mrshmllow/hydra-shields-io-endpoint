use axum::extract::Query;
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use axum::{Json, Router, routing::get};
use futures::future::join_all;
use globset::{Glob, GlobMatcher};
use rayon::prelude::*;
use reqwest::header::{ACCEPT, USER_AGENT};
use reqwest::{StatusCode, Url};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct EndpointResponse {
    schema_version: i8,

    label: String,

    message: String,

    is_error: bool,
}

#[derive(Error, Debug)]
enum EndpointError {
    #[error(transparent)]
    UrlParse(#[from] url::ParseError),

    #[error(transparent)]
    FailedReqwest(#[from] reqwest::Error),
}

impl IntoResponse for EndpointError {
    fn into_response(self) -> axum::response::Response {
        let body = match self {
            Self::UrlParse(error) => axum::Json(EndpointResponse {
                is_error: true,
                label: "URL Parse Error".into(),
                message: error.to_string(),
                ..Default::default()
            }),
            Self::FailedReqwest(error) => axum::Json(EndpointResponse {
                is_error: true,
                label: "URL Rquest Error".into(),
                message: error.to_string(),
                ..Default::default()
            }),
        };

        (StatusCode::INTERNAL_SERVER_ERROR, body).into_response()
    }
}

#[derive(Deserialize, Debug)]
struct RequestQuery {
    hydra_base_url: Url,
    jobsets: Glob,
    jobs: Glob,
}

/// Returned in a list from GET hydra_base_url
#[derive(Serialize, Deserialize, Debug)]
struct Project {
    name: String,
    jobsets: Vec<String>,
}

#[derive(Deserialize, Debug)]
struct JobsetEvaluation {
    id: i32,
    builds: Vec<i32>,
}

/// Returned from GET jobset/:project/:jobset/evals
#[derive(Deserialize, Debug)]
struct JobsetEvalList {
    evals: Vec<JobsetEvaluation>,
}

/// Returned from GET build/:id
#[derive(Deserialize, Debug)]
struct Build {
    job: String,
    finished: i32,
    buildstatus: i32,
}

impl Default for EndpointResponse {
    fn default() -> Self {
        EndpointResponse {
            schema_version: 1,
            is_error: false,
            label: "Default Label".into(),
            message: "Default Message".into(),
        }
    }
}

#[derive(Clone, Debug)]
struct Jobset {
    project: String,
    name: String,
}

impl ToString for Jobset {
    fn to_string(&self) -> String {
        format!("{}:{}", self.project, self.name)
    }
}

fn headers() -> HeaderMap {
    let mut headers = HeaderMap::new();

    headers.insert(ACCEPT, "application/json".parse().unwrap());
    headers.insert(USER_AGENT, "hydra-shields-endpoint".parse().unwrap());

    headers
}

async fn fetch_jobset(
    client: reqwest::Client,
    base_url: Url,
    jobset: Jobset,
) -> Result<JobsetEvalList, EndpointError> {
    let url = base_url.join(&format!("jobset/{}/{}/evals", jobset.project, jobset.name))?;

    let evals = client
        .get(url)
        .headers(headers())
        .send()
        .await?
        .json::<JobsetEvalList>()
        .await?;

    Ok(evals)
}

async fn fetch_build(
    client: reqwest::Client,
    base_url: Url,
    build: i32,
) -> Result<Build, EndpointError> {
    let url = base_url.join(&format!("build/{}", build))?;

    let build = client
        .get(url)
        .headers(headers())
        .send()
        .await?
        .json::<Build>()
        .await?;

    Ok(build)
}

async fn check_jobset_evaluation(
    client: reqwest::Client,
    base_url: Url,
    job_matcher: GlobMatcher,
    evaluation: &JobsetEvaluation,
) -> Result<(bool, bool), EndpointError> {
    let statuses = evaluation
        .builds
        .par_iter()
        .map(|build| fetch_build(client.clone(), base_url.clone(), *build))
        .collect::<Vec<_>>();

    let statuses = join_all(statuses)
        .await
        .into_par_iter()
        .collect::<Result<Vec<_>, EndpointError>>()?;
    let filtered = statuses
        .par_iter()
        .filter(|build| job_matcher.is_match(build.job.clone()))
        .collect::<Vec<_>>();

    if filtered.is_empty() {
        return Ok((false, true));
    }

    let queued = filtered.par_iter().any(|x| x.finished != 1);
    let failure = filtered.par_iter().any(|x| x.buildstatus != 0);

    Ok((queued, failure))
}

async fn check_list_passing(
    client: reqwest::Client,
    base_url: Url,
    job_matcher: GlobMatcher,
    list: &JobsetEvalList,
) -> Result<bool, EndpointError> {
    for evaluation in &list.evals {
        let (queued, failure) = check_jobset_evaluation(
            client.clone(),
            base_url.clone(),
            job_matcher.clone(),
            evaluation,
        )
        .await?;

        if queued { continue; }

        return Ok(!failure);
    }

    Ok(false)
}

#[axum::debug_handler]
async fn endpoint(
    Query(params): Query<RequestQuery>,
) -> Result<Json<EndpointResponse>, EndpointError> {
    let client = reqwest::Client::new();
    let jobset_matcher = params.jobsets.compile_matcher();
    let job_matcher = params.jobs.compile_matcher();

    let projects = client
        .get(params.hydra_base_url.clone())
        .headers(headers())
        .send()
        .await?
        .json::<Vec<Project>>()
        .await?;

    let jobsets = projects
        .par_iter()
        .flat_map(|project| {
            project.jobsets.par_iter().map(|jobset| Jobset {
                project: project.name.clone(),
                name: jobset.to_string(),
            })
        })
        .filter(|x| jobset_matcher.is_match(x.to_string()))
        .map(|jobset| fetch_jobset(client.clone(), params.hydra_base_url.clone(), jobset))
        .collect::<Vec<_>>();

    let jobset_eval_lists: Vec<JobsetEvalList> = join_all(jobsets)
        .await
        .into_par_iter()
        .collect::<Result<_, EndpointError>>()?;

    let passing = jobset_eval_lists.iter().map(|list| {
        check_list_passing(
            client.clone(),
            params.hydra_base_url.clone(),
            job_matcher.clone(),
            list,
        )
    }).collect::<Vec<_>>();

    let jobset_eval_lists: Vec<bool> = join_all(passing)
        .await
        .into_par_iter()
        .collect::<Result<_, EndpointError>>()?;

    if jobset_eval_lists.iter().all(|bool| *bool) {
        return Ok(axum::Json(EndpointResponse {
            label: format!("{}:{}", params.jobsets, params.jobs),
            message: "passing".into(),
            ..Default::default()
        }));
    }

    Ok(axum::Json(EndpointResponse {
        label: format!("{}:{}", params.jobsets, params.jobs),
        message: "one or more jobs failing".into(),
        is_error: true,
        ..Default::default()
    }))
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let app = Router::new().route("/", get(endpoint));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

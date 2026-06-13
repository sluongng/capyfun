//! A blocking REAPI gRPC client for BuildBuddy Cloud.
//!
//! Mirrors the buck2 fork's `re_grpc` client surface, trimmed to what CapyFun
//! needs: CAS (`FindMissingBlobs` / `BatchUpdateBlobs` / `BatchReadBlobs`), the
//! Action Cache (`GetActionResult`), and `Execution.Execute`. Auth is a tonic
//! interceptor injecting `x-buildbuddy-api-key` on every request — the same
//! header buck2's `.buckconfig` configures.
//!
//! CapyFun's engine is synchronous, so this wraps an internal Tokio runtime and
//! exposes blocking methods. The credential is sourced from the environment
//! ([`RemoteConfig::from_env`]); it is **never** committed and **never** enters
//! the Action digest (it rides this header instead — see [`super::action`]).

use anyhow::{anyhow, bail, Context, Result};
use tonic::metadata::{Ascii, MetadataValue};
use tonic::service::interceptor::InterceptedService;
use tonic::service::Interceptor;
use tonic::transport::{Channel, ClientTlsConfig};
use tonic::{Request, Status};

use super::digest::Blob;
use super::proto::google::longrunning::operation;
use super::proto::reapi;

use reapi::action_cache_client::ActionCacheClient;
use reapi::content_addressable_storage_client::ContentAddressableStorageClient;
use reapi::execution_client::ExecutionClient;

const API_KEY_HEADER: &str = "x-buildbuddy-api-key";
const DEFAULT_ENDPOINT: &str = "grpcs://remote.buildbuddy.io";

/// Connection settings for a REAPI backend.
#[derive(Debug, Clone)]
pub struct RemoteConfig {
    /// gRPC endpoint; `grpc(s)://` and `http(s)://` schemes are accepted, a bare
    /// host defaults to TLS.
    pub endpoint: String,
    /// REAPI instance name (BuildBuddy uses the empty default).
    pub instance_name: String,
    /// BuildBuddy API key, or `None` for an unauthenticated endpoint.
    pub api_key: Option<String>,
}

impl RemoteConfig {
    /// Read config from the environment: `BUILDBUDDY_ENDPOINT` (default
    /// `grpcs://remote.buildbuddy.io`), `BUILDBUDDY_INSTANCE_NAME` (default
    /// empty), `BUILDBUDDY_API_KEY` (optional). The key is read at runtime and
    /// never stored in the repo.
    pub fn from_env() -> Self {
        RemoteConfig {
            endpoint: std::env::var("BUILDBUDDY_ENDPOINT")
                .unwrap_or_else(|_| DEFAULT_ENDPOINT.to_owned()),
            instance_name: std::env::var("BUILDBUDDY_INSTANCE_NAME").unwrap_or_default(),
            api_key: std::env::var("BUILDBUDDY_API_KEY").ok().filter(|k| !k.is_empty()),
        }
    }
}

/// Normalize a REAPI endpoint to a tonic-acceptable `http(s)://` URL plus a flag
/// for whether TLS is required. Pure (no I/O), so it is unit-tested directly.
pub fn normalize_endpoint(url: &str) -> (String, bool) {
    if let Some(rest) = url.strip_prefix("grpcs://") {
        (format!("https://{rest}"), true)
    } else if let Some(rest) = url.strip_prefix("grpc://") {
        (format!("http://{rest}"), false)
    } else if url.starts_with("https://") {
        (url.to_owned(), true)
    } else if url.starts_with("http://") {
        (url.to_owned(), false)
    } else {
        // Bare host (e.g. `remote.buildbuddy.io`) → default to TLS.
        (format!("https://{url}"), true)
    }
}

/// Injects the BuildBuddy API key header on every gRPC request.
#[derive(Clone)]
pub struct AuthInterceptor {
    key: Option<MetadataValue<Ascii>>,
}

impl AuthInterceptor {
    fn new(api_key: Option<&str>) -> Result<Self> {
        let key = match api_key {
            Some(k) => Some(
                MetadataValue::try_from(k)
                    .map_err(|_| anyhow!("BUILDBUDDY_API_KEY contains invalid header characters"))?,
            ),
            None => None,
        };
        Ok(AuthInterceptor { key })
    }
}

impl Interceptor for AuthInterceptor {
    fn call(&mut self, mut req: Request<()>) -> std::result::Result<Request<()>, Status> {
        if let Some(k) = &self.key {
            req.metadata_mut().insert(API_KEY_HEADER, k.clone());
        }
        Ok(req)
    }
}

type Service = InterceptedService<Channel, AuthInterceptor>;

fn digest_function() -> i32 {
    reapi::digest_function::Value::Sha256 as i32
}

/// A connected, blocking REAPI client.
pub struct RemoteClient {
    rt: tokio::runtime::Runtime,
    channel: Channel,
    interceptor: AuthInterceptor,
    instance_name: String,
}

impl RemoteClient {
    /// Connect to the configured endpoint (blocking).
    pub fn connect(cfg: &RemoteConfig) -> Result<Self> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .context("building Tokio runtime")?;
        let interceptor = AuthInterceptor::new(cfg.api_key.as_deref())?;

        let (url, tls) = normalize_endpoint(&cfg.endpoint);
        let mut endpoint =
            Channel::from_shared(url.clone()).with_context(|| format!("invalid endpoint {url}"))?;
        if tls {
            endpoint = endpoint
                .tls_config(ClientTlsConfig::new().with_enabled_roots())
                .context("configuring TLS")?;
        }
        let channel = rt
            .block_on(endpoint.connect())
            .with_context(|| format!("connecting to {url}"))?;

        Ok(RemoteClient {
            rt,
            channel,
            interceptor,
            instance_name: cfg.instance_name.clone(),
        })
    }

    fn cas(&self) -> ContentAddressableStorageClient<Service> {
        ContentAddressableStorageClient::with_interceptor(
            self.channel.clone(),
            self.interceptor.clone(),
        )
    }

    fn ac(&self) -> ActionCacheClient<Service> {
        ActionCacheClient::with_interceptor(self.channel.clone(), self.interceptor.clone())
    }

    fn exec(&self) -> ExecutionClient<Service> {
        ExecutionClient::with_interceptor(self.channel.clone(), self.interceptor.clone())
    }

    /// Which of `digests` the CAS is missing (and therefore must be uploaded).
    pub fn find_missing_blobs(&self, digests: Vec<reapi::Digest>) -> Result<Vec<reapi::Digest>> {
        let req = reapi::FindMissingBlobsRequest {
            instance_name: self.instance_name.clone(),
            blob_digests: digests,
            digest_function: digest_function(),
        };
        let resp = self
            .rt
            .block_on(self.cas().find_missing_blobs(req))
            .context("FindMissingBlobs")?;
        Ok(resp.into_inner().missing_blob_digests)
    }

    /// Upload `blobs` to the CAS via `BatchUpdateBlobs`, erroring if the server
    /// reports a non-OK status for any blob.
    pub fn batch_update_blobs(&self, blobs: &[Blob]) -> Result<()> {
        if blobs.is_empty() {
            return Ok(());
        }
        let requests = blobs
            .iter()
            .map(|b| reapi::batch_update_blobs_request::Request {
                digest: Some(b.digest.clone()),
                data: b.data.clone(),
                compressor: reapi::compressor::Value::Identity as i32,
            })
            .collect();
        let req = reapi::BatchUpdateBlobsRequest {
            instance_name: self.instance_name.clone(),
            requests,
            digest_function: digest_function(),
        };
        let resp = self
            .rt
            .block_on(self.cas().batch_update_blobs(req))
            .context("BatchUpdateBlobs")?
            .into_inner();
        for r in resp.responses {
            let status = r.status.unwrap_or_default();
            if status.code != 0 {
                bail!(
                    "CAS rejected blob {}: code {} {}",
                    r.digest.map(|d| d.hash).unwrap_or_default(),
                    status.code,
                    status.message
                );
            }
        }
        Ok(())
    }

    /// Upload only the blobs the CAS does not already have (FindMissingBlobs then
    /// BatchUpdateBlobs). Returns the number of blobs actually uploaded.
    pub fn upload_missing(&self, blobs: &[Blob]) -> Result<usize> {
        let digests = blobs.iter().map(|b| b.digest.clone()).collect();
        let missing = self.find_missing_blobs(digests)?;
        if missing.is_empty() {
            return Ok(0);
        }
        let missing_hashes: std::collections::HashSet<_> =
            missing.iter().map(|d| d.hash.clone()).collect();
        let to_upload: Vec<Blob> = blobs
            .iter()
            .filter(|b| missing_hashes.contains(&b.digest.hash))
            .cloned()
            .collect();
        let n = to_upload.len();
        self.batch_update_blobs(&to_upload)?;
        Ok(n)
    }

    /// Read blobs from the CAS, validating each returned blob's status.
    pub fn batch_read_blobs(&self, digests: Vec<reapi::Digest>) -> Result<Vec<Blob>> {
        if digests.is_empty() {
            return Ok(Vec::new());
        }
        let req = reapi::BatchReadBlobsRequest {
            instance_name: self.instance_name.clone(),
            digests,
            acceptable_compressors: vec![],
            digest_function: digest_function(),
        };
        let resp = self
            .rt
            .block_on(self.cas().batch_read_blobs(req))
            .context("BatchReadBlobs")?
            .into_inner();
        let mut out = Vec::with_capacity(resp.responses.len());
        for r in resp.responses {
            let status = r.status.unwrap_or_default();
            if status.code != 0 {
                bail!(
                    "CAS read failed for {}: code {} {}",
                    r.digest.clone().map(|d| d.hash).unwrap_or_default(),
                    status.code,
                    status.message
                );
            }
            out.push(Blob {
                digest: r.digest.unwrap_or_default(),
                data: r.data,
            });
        }
        Ok(out)
    }

    /// Look up a cached [`ActionResult`](reapi::ActionResult) by Action digest.
    /// `Ok(None)` means a cache miss (gRPC `NOT_FOUND`), not an error.
    pub fn get_action_result(
        &self,
        action_digest: reapi::Digest,
    ) -> Result<Option<reapi::ActionResult>> {
        let req = reapi::GetActionResultRequest {
            instance_name: self.instance_name.clone(),
            action_digest: Some(action_digest),
            digest_function: digest_function(),
            ..Default::default()
        };
        match self.rt.block_on(self.ac().get_action_result(req)) {
            Ok(r) => Ok(Some(r.into_inner())),
            Err(s) if s.code() == tonic::Code::NotFound => Ok(None),
            Err(s) => Err(anyhow!("GetActionResult: {s}")),
        }
    }

    /// Execute an Action and wait for completion, returning the final
    /// [`ExecuteResponse`](reapi::ExecuteResponse). `skip_cache_lookup = true`
    /// forces a fresh run even on an AC hit.
    pub fn execute(
        &self,
        action_digest: reapi::Digest,
        skip_cache_lookup: bool,
    ) -> Result<reapi::ExecuteResponse> {
        let req = reapi::ExecuteRequest {
            instance_name: self.instance_name.clone(),
            action_digest: Some(action_digest),
            skip_cache_lookup,
            digest_function: digest_function(),
            ..Default::default()
        };
        self.rt.block_on(async {
            let mut stream = self.exec().execute(req).await.context("Execute")?.into_inner();
            while let Some(op) = stream.message().await.context("Execute stream")? {
                if !op.done {
                    continue;
                }
                return match op.result {
                    Some(operation::Result::Response(any)) => {
                        prost::Message::decode(any.value.as_slice())
                            .context("decoding ExecuteResponse")
                    }
                    Some(operation::Result::Error(status)) => Err(anyhow!(
                        "remote execution failed: code {} {}",
                        status.code,
                        status.message
                    )),
                    None => bail!("operation completed without a result"),
                };
            }
            bail!("Execute stream ended before the operation completed")
        })
    }
}

#[cfg(test)]
mod tests;

//! Dioxus utilities for the [Axum](https://docs.rs/axum/latest/axum/index.html) server framework.
//!
//! # Example
//! ```rust, no_run
//! #![allow(non_snake_case)]
//! use dioxus::prelude::*;
//!
//! fn main() {
//!     // Hydrate the application on the client
//!     #[cfg(feature = "web")]
//!     dioxus::launch(app);
//!
//!     #[cfg(feature = "server")]
//!     {
//!         tokio::runtime::Runtime::new()
//!             .unwrap()
//!             .block_on(async move {
//!                 let listener = tokio::net::TcpListener::bind("127.0.0.01:8080")
//!                     .await
//!                     .unwrap();
//!                 axum::serve(
//!                         listener,
//!                         axum::Router::new()
//!                             // Server side render the application, serve static assets, and register server functions
//!                             .serve_dioxus_application(ServeConfigBuilder::default(), app)
//!                             .into_make_service(),
//!                     )
//!                     .await
//!                     .unwrap();
//!             });
//!      }
//! }
//!
//! fn app() -> Element {
//!     let mut text = use_signal(|| "...".to_string());
//!
//!     rsx! {
//!         button {
//!             onclick: move |_| async move {
//!                 if let Ok(data) = get_server_data().await {
//!                     text.set(data);
//!                 }
//!             },
//!             "Run a server function"
//!         }
//!         "Server said: {text}"
//!     }
//! }
//!
//! #[server(GetServerData)]
//! async fn get_server_data() -> Result<String, ServerFnError> {
//!     Ok("Hello from the server!".to_string())
//! }
//! ```

#[allow(unused)]
pub(crate) type ContextProviders =
    Arc<Vec<Box<dyn Fn() -> Box<dyn std::any::Any> + Send + Sync + 'static>>>;

use axum::routing::*;
use axum::{
    body::{self, Body},
    extract::State,
    http::{Request, Response, StatusCode},
    response::IntoResponse,
};
use dioxus_lib::prelude::{Element, VirtualDom};
use http::header::*;
use parking_lot::RwLock;

use std::{sync::Arc, task::Poll};

use crate::{
    DioxusServerContext, IncrementalRendererError, ProvideServerContext, ServeConfig, SsrRenderer,
};

/// State used by [`render_handler`] to render a dioxus component with axum
#[derive(Clone)]
pub struct RenderHandleState {
    config: ServeConfig,
    build_virtual_dom: Arc<dyn Fn() -> VirtualDom + Send + Sync>,
    ssr_state: once_cell::sync::OnceCell<Arc<SsrRenderer>>,
}

impl RenderHandleState {
    /// Create a new [`RenderHandleState`]
    pub fn new(config: ServeConfig, root: fn() -> Element) -> Self {
        Self {
            config,
            build_virtual_dom: Arc::new(move || VirtualDom::new(root)),
            ssr_state: Default::default(),
        }
    }

    /// Create a new [`RenderHandleState`] with a custom [`VirtualDom`] factory. This method can be used to pass context into the root component of your application.
    pub fn new_with_virtual_dom_factory(
        config: ServeConfig,
        build_virtual_dom: impl Fn() -> VirtualDom + Send + Sync + 'static,
    ) -> Self {
        Self {
            config,
            build_virtual_dom: Arc::new(build_virtual_dom),
            ssr_state: Default::default(),
        }
    }

    /// Set the [`ServeConfig`] for this [`RenderHandleState`]
    pub fn with_config(mut self, config: ServeConfig) -> Self {
        self.config = config;
        self
    }

    /// Set the [`SSRState`] for this [`RenderHandleState`]. Sharing a [`SSRState`] between multiple [`RenderHandleState`]s is more efficient than creating a new [`SSRState`] for each [`RenderHandleState`].
    pub fn with_ssr_state(mut self, ssr_state: Arc<SsrRenderer>) -> Self {
        self.ssr_state = once_cell::sync::OnceCell::new();
        if self.ssr_state.set(ssr_state).is_err() {
            panic!("SSRState already set");
        }
        self
    }

    fn ssr_state(&self) -> Arc<SsrRenderer> {
        self.ssr_state
            .get_or_init(|| SsrRenderer::shared(self.config.incremental.clone()))
            .clone()
    }
}

/// SSR renderer handler for Axum with added context injection.
///
/// # Example
/// ```rust,no_run
/// #![allow(non_snake_case)]
/// use std::sync::{Arc, Mutex};
///
/// use axum::routing::get;
/// use dioxus::prelude::*;
///
/// fn app() -> Element {
///     rsx! {
///         "hello!"
///     }
/// }
///
/// #[tokio::main]
/// async fn main() {
///     let addr = std::net::SocketAddr::from(([127, 0, 0, 1], 8080));
///     let router = axum::Router::new()
///         // Register server functions, etc.
///         // Note you can use `register_server_functions_with_context`
///         // to inject the context into server functions running outside
///         // of an SSR render context.
///         .fallback(get(render_handler)
///             .with_state(RenderHandleState::new(ServeConfig::new().unwrap(), app))
///         )
///         .into_make_service();
///     let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
///     axum::serve(listener, router).await.unwrap();
/// }
/// ```
pub async fn render_handler(
    State(state): State<RenderHandleState>,
    request: Request<Body>,
) -> impl IntoResponse {
    // Only respond to requests for HTML
    if let Some(mime) = request.headers().get("Accept") {
        match mime.to_str().map(|mime| mime.to_ascii_lowercase()) {
            Ok(accepts) if accepts.contains("text/html") => {}
            _ => return Err(StatusCode::NOT_ACCEPTABLE.into_response()),
        }
    }

    let parts = request.into_parts().0;
    let url = parts
        .uri
        .path_and_query()
        .ok_or(StatusCode::BAD_REQUEST.into_response())?
        .to_string();

    let server_context = DioxusServerContext::from_shared_parts(Arc::new(RwLock::new(parts)));
    let build_virtual_dom = state.build_virtual_dom.clone();

    let stream = state
        .ssr_state()
        .render_to(
            state.config.clone(),
            url,
            move || build_virtual_dom(),
            server_context.clone(),
        )
        .await
        .map_err(|err| {
            Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(body::Body::new(format!("Error: {}", err)))
                .unwrap()
        })?;

    let mut response = stream.into_response();

    for (key, value) in server_context.response_parts().headers.iter() {
        response.headers_mut().insert(key, value.clone());
    }

    Ok(response)
}

/// A handler for Dioxus server functions. This will run the server function and return the result.
pub async fn handle_server_fns_inner(
    path: &str,
    additional_context: impl Fn(&DioxusServerContext) + 'static + Clone + Send,
    req: Request<Body>,
) -> impl IntoResponse {
    use server_fn::middleware::Service;

    let path_string = path.to_string();

    let future = move || async move {
        let (parts, body) = req.into_parts();
        let req = Request::from_parts(parts.clone(), body);

        if let Some(mut service) =
            server_fn::axum::get_server_fn_service(&path_string)
        {
            let server_context = DioxusServerContext::new(parts);
            additional_context(&server_context);

            // store Accepts and Referrer in case we need them for redirect (below)
            let accepts_html = req
                .headers()
                .get(ACCEPT)
                .and_then(|v| v.to_str().ok())
                .map(|v| v.contains("text/html"))
                .unwrap_or(false);
            let referrer = req.headers().get(REFERER).cloned();

            // actually run the server fn (which may use the server context)
            let mut res = ProvideServerContext::new(service.run(req), server_context.clone()).await;

            // it it accepts text/html (i.e., is a plain form post) and doesn't already have a
            // Location set, then redirect to Referer
            if accepts_html {
                if let Some(referrer) = referrer {
                    let has_location = res.headers().get(LOCATION).is_some();
                    if !has_location {
                        *res.status_mut() = StatusCode::FOUND;
                        res.headers_mut().insert(LOCATION, referrer);
                    }
                }
            }

            // apply the response parts from the server context to the response
            let mut res_options = server_context.response_parts_mut();
            res.headers_mut().extend(res_options.headers.drain());

            Ok(res)
        } else {
            Response::builder().status(StatusCode::BAD_REQUEST).body(
                {
                    #[cfg(target_family = "wasm")]
                    {
                        Body::from(format!(
                            "No server function found for path: {path_string}\nYou may need to explicitly register the server function with `register_explicit`, rebuild your wasm binary to update a server function link or make sure the prefix your server and client use for server functions match.",
                        ))
                    }
                    #[cfg(not(target_family = "wasm"))]
                    {
                        Body::from(format!(
                            "No server function found for path: {path_string}\nYou may need to rebuild your wasm binary to update a server function link or make sure the prefix your server and client use for server functions match.",
                        ))
                    }
                }
            )
        }
        .expect("could not build Response")
    };
    #[cfg(target_arch = "wasm32")]
    {
        use futures_util::future::FutureExt;

        let result = tokio::task::spawn_local(future);
        let result = result.then(|f| async move { f.unwrap() });
        result.await.unwrap_or_else(|e| {
            use server_fn::error::NoCustomError;
            use server_fn::error::ServerFnErrorSerde;
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                ServerFnError::<NoCustomError>::ServerError(e.to_string())
                    .ser()
                    .unwrap_or_default(),
            )
                .into_response()
        })
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        future().await
    }
}
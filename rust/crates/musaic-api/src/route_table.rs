//! The router's route table.
//!
//! axum's `Router` cannot list its routes, so every route is registered
//! through [`Table::route`] with an [`Endpoint`] built by this module's
//! [`get`] / [`post`] / ... (and [`Endpoint::get`] / ...). The same call
//! that adds a handler to the `MethodRouter` records its method, so the
//! table cannot drift from what is registered. [`crate::route_table`]
//! returns it; `tests/inventory.rs` compares it with the Python app's OpenAPI
//! inventory (testdata/golden/api/routes.json).
//!
//! Query parameters are recorded too: [`Endpoint::query`] declares the
//! names the handler last added reads. The declaration rides on the request
//! ([`QueryDecl`]) and [`crate::lax::QueryParams::get`] refuses, in debug
//! builds (every test and any debug binary), to read a name its route
//! did not declare, so a handler reading an undeclared parameter fails the
//! first test that reaches it.

use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, FromRequestParts, Query};
use axum::handler::Handler;
use axum::http::Method;
use axum::http::request::Parts;
use axum::routing::{MethodFilter, MethodRouter};
use axum::{Extension, Router};

use crate::state::AppState;

/// One registered (method, path) and the query parameters its handler
/// reads. `path` is axum's spelling (`{*full_path}` for a catch-all).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteRow {
    pub method: &'static str,
    pub path: &'static str,
    pub query: &'static [&'static str],
}

/// The query parameters declared for each method of one path, carried as a
/// request extension.
#[derive(Debug, Clone)]
pub struct QueryDecl(Arc<Vec<(&'static str, &'static [&'static str])>>);

impl QueryDecl {
    fn for_method(&self, m: &Method) -> Option<&'static [&'static str]> {
        // axum's GET route also answers HEAD.
        let m = if m == Method::HEAD { "GET" } else { m.as_str() };
        self.0.iter().find(|(name, _)| *name == m).map(|(_, q)| *q)
    }
}

/// The handlers of one path, with the methods (and their declared query
/// parameters) recorded as they are added.
pub struct Endpoint {
    mr: MethodRouter<AppState>,
    methods: Vec<(&'static str, &'static [&'static str])>,
}

fn filter(m: &'static str) -> MethodFilter {
    match m {
        "GET" => MethodFilter::GET,
        "POST" => MethodFilter::POST,
        "PUT" => MethodFilter::PUT,
        "PATCH" => MethodFilter::PATCH,
        "DELETE" => MethodFilter::DELETE,
        other => panic!("unexpected method {other}"),
    }
}

impl Endpoint {
    fn empty() -> Self {
        Endpoint {
            mr: MethodRouter::new(),
            methods: Vec::new(),
        }
    }

    /// `method` answered by `h` (axum's `on`; GET also answers HEAD, which
    /// `starlette_routing` refuses before routing).
    pub fn on<H, T>(mut self, method: &'static str, h: H) -> Self
    where
        H: Handler<T, AppState>,
        T: 'static,
    {
        assert!(
            !self.methods.iter().any(|(m, _)| *m == method),
            "{method} registered twice on one path"
        );
        self.mr = self.mr.on(filter(method), h);
        self.methods.push((method, &[]));
        self
    }

    pub fn get<H: Handler<T, AppState>, T: 'static>(self, h: H) -> Self {
        self.on("GET", h)
    }
    pub fn post<H: Handler<T, AppState>, T: 'static>(self, h: H) -> Self {
        self.on("POST", h)
    }
    pub fn put<H: Handler<T, AppState>, T: 'static>(self, h: H) -> Self {
        self.on("PUT", h)
    }
    pub fn patch<H: Handler<T, AppState>, T: 'static>(self, h: H) -> Self {
        self.on("PATCH", h)
    }
    pub fn delete<H: Handler<T, AppState>, T: 'static>(self, h: H) -> Self {
        self.on("DELETE", h)
    }

    /// The query parameters the handler added last reads (by name, through
    /// [`crate::lax::QueryParams`]).
    pub fn query(mut self, names: &'static [&'static str]) -> Self {
        let last = self.methods.last_mut().expect("query() after a handler");
        last.1 = names;
        self
    }

    /// No body limit on every method of this path (the handler caps it).
    pub fn no_body_limit(mut self) -> Self {
        self.mr = self.mr.layer(DefaultBodyLimit::disable());
        self
    }
}

pub fn get<H: Handler<T, AppState>, T: 'static>(h: H) -> Endpoint {
    Endpoint::empty().get(h)
}
pub fn post<H: Handler<T, AppState>, T: 'static>(h: H) -> Endpoint {
    Endpoint::empty().post(h)
}
pub fn put<H: Handler<T, AppState>, T: 'static>(h: H) -> Endpoint {
    Endpoint::empty().put(h)
}
pub fn delete<H: Handler<T, AppState>, T: 'static>(h: H) -> Endpoint {
    Endpoint::empty().delete(h)
}
pub fn on<H: Handler<T, AppState>, T: 'static>(method: &'static str, h: H) -> Endpoint {
    Endpoint::empty().on(method, h)
}

/// A `Router` under construction and the rows registered so far.
pub struct Table {
    router: Router<AppState>,
    rows: Vec<RouteRow>,
}

impl Default for Table {
    fn default() -> Self {
        Table {
            router: Router::new(),
            rows: Vec::new(),
        }
    }
}

impl Table {
    pub fn route(mut self, path: &'static str, ep: Endpoint) -> Self {
        assert!(
            !self.rows.iter().any(|r| r.path == path),
            "{path} registered twice"
        );
        for (method, query) in &ep.methods {
            self.rows.push(RouteRow {
                method,
                path,
                query,
            });
        }
        let decl = QueryDecl(Arc::new(ep.methods));
        self.router = self.router.route(path, ep.mr.layer(Extension(decl)));
        self
    }

    pub fn finish(self) -> (Router<AppState>, Vec<RouteRow>) {
        (self.router, self.rows)
    }
}

/// The query string as (key, value) pairs in request order, with the names
/// the matched route declared for this method (`None` outside the table).
pub struct Q(
    pub Vec<(String, String)>,
    pub Option<&'static [&'static str]>,
);

impl<S: Send + Sync> FromRequestParts<S> for Q {
    type Rejection = axum::extract::rejection::QueryRejection;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let Query(pairs) = Query::<Vec<(String, String)>>::from_request_parts(parts, state).await?;
        let declared = parts
            .extensions
            .get::<QueryDecl>()
            .and_then(|d| d.for_method(&parts.method));
        Ok(Q(pairs, declared))
    }
}

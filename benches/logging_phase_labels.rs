//! Micro-benchmark for the label-extraction pattern in
//! `src/proxy/logging_phase.rs::logging()` (#88).
//!
//! `before_88` mirrors the original hot path: `method`/`status`/`route` are
//! captured as owned `String`s (`.to_owned()` / `.to_string()`), and
//! `status_u16` is re-derived via `.parse::<u16>()` at each of the three
//! call sites that need it (`upstream_errors_total`,
//! `record_response_status`, the OTel span).
//!
//! `after_88` mirrors the refactored hot path: `method`/`status`/`route`
//! are borrowed `&str`s (`Method::as_str()`, `StatusCode::as_str()`,
//! `Uri::path()`), and `status_u16` is computed once via `StatusCode::as_u16()`
//! and reused. `status: &StatusCode` (rather than by value) mirrors
//! `session.response_written()` returning `Option<&ResponseHeader>`, so
//! `status.as_str()` borrows from the caller for as long as `method`/`route` do.

use criterion::{criterion_group, criterion_main, Criterion};
use http::{Method, StatusCode, Uri};
use std::hint::black_box;

#[allow(clippy::type_complexity)]
fn before_88(
    method: &Method,
    status: &StatusCode,
    uri: &Uri,
) -> (String, String, String, u16, u16, u16) {
    let method = method.as_str().to_owned();
    let status_str = status.as_u16().to_string();
    let route = uri.path().to_owned();

    // Three call sites in the pre-#88 code each re-derive status_u16 from
    // the formatted status string.
    let upstream_errors = status_str.parse::<u16>().unwrap_or(0);
    let response_status = status_str.parse::<u16>().unwrap_or(0);
    let otel_status = status_str.parse::<u16>().unwrap_or(0);

    (
        method,
        status_str,
        route,
        upstream_errors,
        response_status,
        otel_status,
    )
}

#[allow(clippy::type_complexity)]
fn after_88<'a>(
    method: &'a Method,
    status: &'a StatusCode,
    uri: &'a Uri,
) -> (&'a str, &'a str, &'a str, u16, u16, u16) {
    let method = method.as_str();
    let status_u16 = status.as_u16();
    let status_str = status.as_str();
    let route = uri.path();

    (
        method, status_str, route, status_u16, status_u16, status_u16,
    )
}

fn bench_logging_labels(c: &mut Criterion) {
    let method = Method::GET;
    let status = StatusCode::OK;
    let uri: Uri = "/api/v1/users/12345".parse().unwrap();

    let mut group = c.benchmark_group("logging_phase_labels");

    group.bench_function("before_88_owned", |b| {
        b.iter(|| {
            black_box(before_88(
                black_box(&method),
                black_box(&status),
                black_box(&uri),
            ))
        })
    });

    group.bench_function("after_88_borrowed", |b| {
        b.iter(|| {
            black_box(after_88(
                black_box(&method),
                black_box(&status),
                black_box(&uri),
            ))
        })
    });

    group.finish();
}

criterion_group!(benches, bench_logging_labels);
criterion_main!(benches);

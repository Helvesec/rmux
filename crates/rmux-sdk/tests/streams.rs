//! Public pane-output stream API checks.
//!
//! Wire-level cursor, lag, and drop-guard contract tests live inside the
//! crate so they can use crate-private transport hooks without exposing
//! test-only constructors on the SDK surface.

#![cfg(any(unix, windows))]

use std::fmt::Debug;

use rmux_sdk::{
    PaneLagNotice, PaneLineItem, PaneLineStream, PaneOutputChunk, PaneOutputStart,
    PaneOutputStream, PaneRecentOutput, PaneRenderStream, RenderUpdate,
};

fn assert_send<T: Send>() {}
fn assert_send_sync<T: Send + Sync>() {}
fn assert_static<T: 'static>() {}
fn assert_debug<T: Debug>() {}

#[test]
fn pane_output_stream_public_types_are_send_static_and_debuggable() {
    assert_send::<PaneOutputStream>();
    assert_static::<PaneOutputStream>();
    assert_debug::<PaneOutputStream>();

    assert_send::<PaneLineStream>();
    assert_static::<PaneLineStream>();
    assert_debug::<PaneLineStream>();

    assert_send::<PaneOutputChunk>();
    assert_static::<PaneOutputChunk>();
    assert_debug::<PaneOutputChunk>();

    assert_send::<PaneLineItem>();
    assert_static::<PaneLineItem>();
    assert_debug::<PaneLineItem>();

    assert_send::<PaneLagNotice>();
    assert_static::<PaneLagNotice>();
    assert_debug::<PaneLagNotice>();

    assert_send::<PaneRecentOutput>();
    assert_static::<PaneRecentOutput>();
    assert_debug::<PaneRecentOutput>();

    assert_send::<PaneOutputStart>();
    assert_static::<PaneOutputStart>();
    assert_debug::<PaneOutputStart>();

    assert_send_sync::<PaneRenderStream>();
    assert_static::<PaneRenderStream>();
    assert_debug::<PaneRenderStream>();

    assert_send::<RenderUpdate>();
    assert_static::<RenderUpdate>();
    assert_debug::<RenderUpdate>();
}

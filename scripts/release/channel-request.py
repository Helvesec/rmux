#!/usr/bin/env python3
"""Command-line entry point for downstream channel request evidence."""

from channel_request import pre_site_summary_digest, run

__all__ = ["pre_site_summary_digest"]


if __name__ == "__main__":
    raise SystemExit(run())

#!/bin/bash
# Layout regression tests. Use an isolated data directory and build directory.
set -euo pipefail
: "${GRAINEO_DATA_DIR:?GRAINEO_DATA_DIR must point to a dedicated test directory}"
: "${CARGO_TARGET_DIR:?CARGO_TARGET_DIR must point to a dedicated build directory}"
export GRAINEO_NO_PHOTOKIT=1 GRAINEO_LIBRARY_NO_SYNC=1 GRAINEO_LIBRARY_SIDECARS=0
cargo test -j 4 -p graineo bounded_section -- --test-threads=1
cargo test -j 4 -p graineo long_metadata_stays_within_its_row -- --test-threads=1
cargo test -j 4 -p graineo metadata_rows_share_one_axis -- --test-threads=1
cargo test -j 4 -p graineo content_keeps_clear_of_the_scroll_bar -- --test-threads=1
cargo test -j 4 -p graineo info_rows -- --test-threads=1
cargo test -j 4 -p graineo standard_dialog_bounds_long_content_in_all_languages -- --test-threads=1
cargo test -j 4 -p graineo long_filename_keeps_tool_panels_inside_the_window -- --test-threads=1
cargo test -j 4 -p graineo attribute_groups_do_not_overlap_at_narrow_widths -- --test-threads=1
cargo test -j 4 -p graineo central_settings_fit_the_minimum_viewport_in_both_languages -- --test-threads=1
cargo test -j 4 -p graineo category_menu_follows_display_language_alphabetically -- --test-threads=1

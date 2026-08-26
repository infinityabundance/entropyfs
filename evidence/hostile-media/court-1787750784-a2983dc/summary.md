# Hostile-media court — Wed 26 Aug 13:27:56 UTC 2026

- revision: `a2983dc`
- kernel: `7.2.0-1-cachyos`
- unix: 1787750784
- cases: descriptor=200000 graph=200000 store=30000
- suites passed: 4 / failed: 0

## Admission: PASS

Every court and the full lib suite pass. The court's
resource-bounds claim is therefore implemented.

## Raw output

```text
entropyfs hostile-media court — Wed 26 Aug 13:26:24 UTC 2026
revision: a2983dc
kernel: 7.2.0-1-cachyos
cases: descriptor=200000 graph=200000 store=30000


== descriptor court (200000 cases/proptest target) ==
   Compiling entropyfs v0.7.0 (/mnt/1tb_kingston/entropyfs)
warning: struct `NoProvider` is never constructed
   --> src/tests/io_backend_parity.rs:171:24
    |
171 |                 struct NoProvider;
    |                        ^^^^^^^^^^
    |
    = note: `#[warn(dead_code)]` (part of `#[warn(unused)]`) on by default

warning: `entropyfs` (lib test) generated 1 warning
    Finished `release` profile [optimized + debuginfo] target(s) in 49.07s
     Running unittests src/lib.rs (target/release/deps/entropyfs-fa46debff7628dae)

running 10 tests
test tests::hostile_media::descriptor_court::descriptor_cap_boundary ... ok
test tests::hostile_media::descriptor_court::seeds_are_canonical_and_valid ... ok
test tests::hostile_media::descriptor_court::seeds_bounded_under_tight_limits ... ok
test tests::hostile_media::descriptor_court::truncation_at_every_boundary_of_every_seed ... ok
test tests::hostile_media::descriptor_court::exhibits_never_panic ... ok
test tests::hostile_media::descriptor_court::descriptor_exhibits_pass ... ok
test tests::hostile_media::descriptor_court::trailing_garbage_oracle ... ok
test tests::hostile_media::descriptor_court::slice_oracle ... ok
test tests::hostile_media::descriptor_court::mutated_seeds_oracle ... ok
test tests::hostile_media::descriptor_court::uniform_noise_oracle ... ok

test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 403 filtered out; finished in 0.58s


== graph court (200000 cases/proptest target) ==
warning: struct `NoProvider` is never constructed
   --> src/tests/io_backend_parity.rs:171:24
    |
171 |                 struct NoProvider;
    |                        ^^^^^^^^^^
    |
    = note: `#[warn(dead_code)]` (part of `#[warn(unused)]`) on by default

warning: `entropyfs` (lib test) generated 1 warning
    Finished `release` profile [optimized + debuginfo] target(s) in 0.04s
     Running unittests src/lib.rs (target/release/deps/entropyfs-fa46debff7628dae)

running 6 tests
test tests::hostile_media::graph_court::graph_exhibits_pass ... ok
test tests::hostile_media::graph_court::graph_seeds_bounded_under_tight_limits ... ok
test tests::hostile_media::graph_court::graph_seeds_materialize_to_pinned_content ... ok
test tests::hostile_media::graph_court::mutated_graphs_oracle ... ok
test tests::hostile_media::graph_court::spliced_graphs_oracle ... ok
test tests::hostile_media::graph_court::noise_graphs_oracle ... ok

test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 407 filtered out; finished in 3.02s


== store court (30000 cases/proptest target) ==
warning: struct `NoProvider` is never constructed
   --> src/tests/io_backend_parity.rs:171:24
    |
171 |                 struct NoProvider;
    |                        ^^^^^^^^^^
    |
    = note: `#[warn(dead_code)]` (part of `#[warn(unused)]`) on by default

warning: `entropyfs` (lib test) generated 1 warning
    Finished `release` profile [optimized + debuginfo] target(s) in 0.04s
     Running unittests src/lib.rs (target/release/deps/entropyfs-fa46debff7628dae)

running 12 tests
test tests::hostile_media::store_court::mutation_log_duplicate_and_nonmonotonic_sequences ... ok
test tests::hostile_media::store_court::physical_splices_are_bounded ... ok
test tests::hostile_media::store_court::valid_crc_envelope_containing_malicious_descriptor ... ok
test tests::hostile_media::store_court::diamond_deepest_path_chain_depth ... ok
test tests::hostile_media::store_court::semantic_superblock_patch_is_bounded ... ok
test tests::hostile_media::store_court::btree_fanout_and_key_exhibits ... ok
test tests::hostile_media::store_court::physical_payload_flips_are_integrity_rejected ... ok
test tests::hostile_media::store_court::semantic_payload_rewrites_are_bounded ... ok
test tests::hostile_media::store_court::physical_header_flips_and_truncation_are_bounded ... ok
test tests::hostile_media::store_court::semantic_record_mutations_are_bounded ... ok
test tests::hostile_media::store_court::whole_store_mutator ... ok
test tests::hostile_media::store_court::superblock_mutator ... ok

test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 401 filtered out; finished in 34.00s


== full lib suite ==
warning: struct `NoProvider` is never constructed
   --> src/tests/io_backend_parity.rs:171:24
    |
171 |                 struct NoProvider;
    |                        ^^^^^^^^^^
    |
    = note: `#[warn(dead_code)]` (part of `#[warn(unused)]`) on by default

warning: `entropyfs` (lib test) generated 1 warning
    Finished `release` profile [optimized + debuginfo] target(s) in 0.05s
     Running unittests src/lib.rs (target/release/deps/entropyfs-fa46debff7628dae)

running 413 tests
test cache::materialized::tests::oversized_single_entry ... ok
test cache::materialized::tests::lru_eviction ... ok
test cache::materialized::tests::replace_updates_size ... ok
test cache::metadata::tests::bounded_and_lru ... ok
test cache::model::tests::memoizes_decoded_models ... ok
test core::candidate::tests::pick_cheapest_prefers_zero ... ok
test core::candidate::tests::raw_candidate_always_valid ... ok
test core::cost::split_tests::residual_split_rules ... ok
test core::candidate::tests::zero_candidate_only_for_zeros ... ok
test core::cost::split_tests::split_sums_match_encoded_plus_model ... ok
test core::cost::tests::latency_policy_prefers_cheap_reads ... ok
test core::cost::tests::raw_dominates_random_under_capacity ... ok
test core::extent::tests::chunk_id_roundtrip ... ok
test core::extent::tests::extent_arithmetic ... ok
test core::limits::tests::defaults_are_consistent ... ok
test core::materialize::tests::base_residual_xor ... ok
test core::materialize::tests::depth_cap_enforced ... ok
test core::materialize::tests::exact_ref_subrange ... ok
test core::materialize::tests::inline_and_raw ... ok
test core::materialize::tests::range_replace_residual ... ok
test core::materialize::tests::sparse_roundtrip_via_engine ... ok
test core::materialize::tests::zero_and_fill ... ok
test core::materialize::tests::work_budget_exceeded ... ok
test core::representation::tests::periodic_validation ... ok
test core::representation::tests::residual_edits_sorted ... ok
test core::representation::tests::sparse_validation ... ok
test core::representation::tests::zero_too_large_rejected ... ok
test core::representation::tests::zero_valid ... ok
test dsfb::drift::tests::classification ... ok
test dsfb::drift::tests::tracker_drift_on_gradual_decline ... ok
test dsfb::drift::tests::tracker_slew_on_jump_with_persistence ... ok
test dsfb::drift::tests::tracker_stable_on_constant ... ok
test dsfb::observer::tests::drift_keeps_narrow ... ok
test dsfb::observer::tests::slew_broadens_search ... ok
test dsfb::selection::tests::plan_order_is_trust_descending ... ok
test dsfb::observer::tests::stable_evidence_keeps_trust_high ... ok
test dsfb::observer::tests::eviction_bounds_state ... ok
test dsfb::slew::tests::detects_jump ... ok
test dsfb::slew::tests::ignores_gradual_change ... ok
test entropy::coordinate::tests::combination_coordinate ... ok
test dsfb::trust::tests::breadth_budget_ordering ... ok
test entropy::coordinate::tests::factoradic_coordinate ... ok
test entropy::coordinate::tests::multinomial_coordinate ... ok
test entropy::palette::tests::palette_skips_high_cardinality ... ok
test entropy::periodic::tests::periodic_roundtrip ... ok
test entropy::periodic::tests::periodic_skips_nonperiodic ... ok
test entropy::palette::tests::palette_encoder_roundtrip ... ok
test entropy::palette::tests::palette_skips_overflowing_state_space ... ok
test entropy::periodic::tests::smallest_period_wins ... ok
test entropy::periodic::tests::periodic_with_tail ... ok
test entropy::permutation::tests::permutation_roundtrip ... ok
test entropy::permutation::tests::permutation_skips_duplicates_and_large ... ok
test entropy::rank::tests::comb_basics ... ok
test entropy::rank::tests::comb_fits_u128_boundary ... ok
test entropy::rank::tests::multinomial_counts ... ok
test entropy::rank::tests::comb_symmetry ... ok
test entropy::rank::tests::multinomial_out_of_range ... ok
test entropy::rank::tests::multinomial_roundtrip_exhaustive_small ... ok
test entropy::rank::tests::permutation_size_cap ... ok
test entropy::rank::tests::subset_rank_overflow_rejected ... ok
test entropy::rank::tests::subset_roundtrip_small ... ok
test entropy::rank::tests::unrank_34_fits ... ok
test entropy::rank::tests::subset_roundtrip_exhaustive_k1 ... ok
test entropy::residual::tests::dense_run_uses_range ... ok
test entropy::residual::tests::fanout_cap ... ok
test entropy::residual::tests::identical_targets ... ok
test entropy::residual::tests::sparse_diffs ... ok
test entropy::sparse::tests::sparse_encoder_proposes_and_roundtrips ... ok
test entropy::sparse64::tests::small_k_delegates_to_sparse ... ok
test entropy::sparse::tests::sparse_skips_zero_input ... ok
test entropy::sparse::tests::sparse_skips_dense ... ok
test entropy::transform::tests::identity ... ok
test entropy::universe::tests::differs_across_coordinates_and_seeds ... ok
test entropy::sparse64::tests::dense_and_zero_skip ... ok
test evidence::casefile::tests::corrupt_byte_detected ... ok
test evidence::casefile::tests::roundtrip_and_verify ... ok
test evidence::casefile::tests::truncated_rejected ... ok
test evidence::environment::tests::percentile_nearest_rank ... ok
test evidence::environment::tests::summary_basics ... ok
test evidence::environment::tests::disk_delta_saturates ... ok
test evidence::receipt::tests::receipt_json_roundtrip ... ok
test evidence::manifest::tests::manifest_json_roundtrip ... ok
test format::codec::tests::length_prefixed ... ok
test format::codec::tests::crc ... ok
test format::codec::tests::int_roundtrip ... ok
test evidence::environment::tests::mount_of_returns_something ... ok
test entropy::universe::tests::range_matches_full ... ok
test entropy::universe::tests::deterministic ... ok
test entropy::rank::tests::permutation_roundtrip ... ok
test format::codec::tests::too_long_capped ... ok
test format::codec::tests::varint_roundtrip ... ok
test format::descriptor::tests::encoded_size_matches_codec ... ok
test format::descriptor::tests::oversized_descriptor_rejected ... ok
test format::descriptor::tests::corrupt_descriptors_error ... ok
test format::record::tests::content_id_must_match ... ok
test format::record::tests::roundtrip_with_and_without_materialized_len ... ok
test evidence::corpus::tests::urandom_is_deterministic ... ok
test format::descriptor::tests::truncated_errors ... ok
test format::descriptor::tests::roundtrip_all_families ... ok
test format::codec::tests::truncated_errors ... ok
test format::descriptor::tests::roundtrip_randomized ... ok
test entropy::sparse64::tests::overflow_range_roundtrips ... ok
test format::features::tests::compat_checks ... ok
test format::record::tests::sequential_walk ... ok
test format::features::tests::feature_sets ... ok
test format::record::tests::zero_tail_is_end ... ok
test format::record::tests::truncated_tail ... ok
test format::superblock::tests::roundtrip ... ok
test format::superblock::tests::corrupt_slot_rejected ... ok
test format::superblock::tests::slot_selection ... ok
test fuse::locking::tests::unlock_and_getlk ... ok
test fuse::inode::tests::attr_roundtrip_fields ... ok
test fuse::locking::tests::flush_releases_owner ... ok
test fuse::locking::tests::read_locks_share ... ok
test fuse::directory::tests::entry_list_has_dot_dotdot_and_sorted_entries ... ok
test fuse::locking::tests::write_lock_blocks_read_and_write ... ok
test fuse::file::tests::aligned_full_chunk ... ok
test evidence::corpus::tests::structured_is_reproducible ... ok
test evidence::corpus::tests::splitmix_stream_is_incompressible_looking ... ok
test integrity::content::tests::content_id_distinguishes_bytes ... ok
test fuse::xattr::tests::namespace_policy ... ok
test integrity::content::tests::verify_descriptor_rejects_wrong_key ... ok
test integrity::root::tests::id_mismatch_detected ... ok
test integrity::record::tests::envelope_verify_detects_flip ... ok
test integrity::root::tests::root_payload_binding ... ok
test optimizer::foreground::tests::tiny_chunks_never_skip ... ok
test optimizer::search::tests::base_depth_accounted_in_costs ... ok
test fuse::file::tests::hole_write_extends_size ... ok
test optimizer::foreground::tests::entropy_classification_is_deterministic_and_sane ... ok
test platform::linux::tests::page_size_sane ... ok
test platform::io_uring::tests::nop_completes ... ok
test platform::linux::tests::path_containment ... ok
test optimizer::search::tests::ablation_raw_only_never_dedups_or_compresses ... ok
test rans::delta::tests::delta_candidate_roundtrips_and_wins ... ok
test rans::dispatch::tests::authority_roundtrip ... ok
test rans::dispatch::tests::backend_codec_mismatch_rejected ... ok
test rans::metadata::tests::corrupt_bytes_typed_error ... ok
test platform::io_uring::tests::batch_out_of_order_ok ... ok
test rans::metadata::tests::roundtrip ... ok
test optimizer::search::tests::prev_version_base_wins_for_tiny_edit ... ok
test rans::metadata::tests::roundtrip_skewed ... ok
test rans::metadata::tests::size_limit_enforced ... ok
test rans::metadata::tests::truncated_typed_error ... ok
test rans::metadata::tests::uniform_model_is_small ... ok
test rans::delta::tests::inserted_line_shift_delta_is_tiny ... ok
test rans::model::tests::entropy_of_uniform_is_8 ... ok
test rans::model::tests::expected_len_low_for_skewed ... ok
test rans::delta::tests::delta_skips_unrelated_base ... ok
test optimizer::search::tests::random_data_has_no_fake_density ... ok
test optimizer::search::tests::dedup_wins_for_duplicate_content ... ok
test rans::metadata::tests::model_id_is_content_addressed ... ok
test rans::model::tests::degenerate_models_rejected ... ok
test rans::model::tests::symbols_rebuild ... ok
test evidence::corpus::tests::versioned_has_distinct_versions ... ok
test rans::model::tests::normalization_deterministic ... ok
test rans::model::tests::normalization_sums_to_scale ... ok
test rans::residual::tests::corrupt_stream_errors ... ok
test platform::io_uring::tests::write_read_roundtrip ... ok
test rans::model::tests::every_present_symbol_has_frequency ... ok
test optimizer::search::tests::oversized_descriptor_candidate_is_rejected ... ok
test rans::residual::tests::rans_residual_encoder_proposes ... ok
test rans::residual::tests::rans_skips_incompressible ... ok
test optimizer::search::tests::guided_search_matches_exact_bytes ... ok
test rans::residual::tests::interleaved2_roundtrip ... ok
test rans::residual::tests::rans_encoder_proposes_and_validates ... ok
test rans::sequence::tests::deep_repcodes_repeat_short_matches_at_same_distance ... ok
test rans::sequence::tests::deep_reserved_command_is_rejected_by_validate ... ok
test rans::sequence::tests::deep_extended_length_covers_long_match ... ok
test rans::sequence::tests::deep_parse_roundtrips_exactly ... ok
test fuse::file::tests::partial_chunk_rmw_preserves_neighbors ... ok
test rans::sequence::tests::dict_dictionary_must_be_bounded ... ok
test rans::sequence::tests::degenerate_streams_fall_back_to_raw_slots ... ok
test rans::residual::tests::truncated_stream_errors ... ok
test rans::residual::tests::single_roundtrip ... ok
test rans::sequence::tests::dict_long_match_continuation_advances_offset ... ok
test rans::sequence::tests::deep_uses_repcodes_on_rle ... ok
test fuse::file::tests::cross_chunk_write ... ok
test rans::sequence::tests::deep_encoder_wins_and_validates ... ok
test rans::sequence::tests::dict_depth_cap_refuses_candidate ... ok
test rans::sequence::tests::dict_skips_unrelated_dictionary ... ok
test rans::sequence::tests::long_match_continues_at_same_distance ... ok
test rans::sequence::tests::model_object_roundtrip ... ok
test rans::sequence::tests::rle_is_all_copies_after_prefix ... ok
test optimizer::search::tests::self_referential_base_is_rejected ... ok
test rans::sequence::tests::literal_only_input ... ok
test rans::sequence::tests::rle_slot_layout ... ok
test rans::sequence::tests::mulshift_data_roundtrips_exactly ... ok
test rans::sequence::tests::external_model_without_symbol_coverage_falls_back_to_raw ... ok
test rans::sequence::tests::deep_repcodes_survive_store_roundtrip_via_materialize ... ok
test rans::sequence::tests::deep_skips_urandom ... ok
test rans::sequence::tests::dict_encoder_wins_and_validates ... ok
test rans::sequence::tests::sequence_encoder_wins_on_text ... ok
test rans::sequence::tests::sequence_skips_urandom ... ok
test rans::sequence::tests::dict_parse_uses_dictionary_and_roundtrips_exactly ... ok
test rans::sequence::tests::shared_dictionary_must_be_bounded ... ok
test rans::sequence::tests::shared_depth_cap_refuses_candidate ... ok
test store::directory::tests::insert_lookup_remove_scan ... ok
test store::directory::tests::raw_bytes_names ... ok
test store::extent_tree::tests::insert_covering_scan_remove ... ok
test rans::sequence::tests::shared_long_match_continuation_advances_offset ... ok
test rans::sequence::tests::shared_skips_unrelated_dictionary ... ok
test store::index::tests::apply_sorted_batch_collapses_to_empty ... ok
test rans::sequence::tests::shared_encoder_wins_and_validates ... ok
test store::inode::tests::dir_inode_roundtrip ... ok
test store::index::tests::corrupt_node_typed_error ... ok
test rans::sequence::tests::shared_three_way_parse_uses_all_sources ... ok
test store::inode::tests::inconsistent_type_rejected ... ok
test store::inode::tests::file_inode_roundtrip ... ok
test store::inode::tests::symlink_roundtrip ... ok
test store::io::sync::tests::delete_evicts_handle ... ok
test store::io::sync::tests::open_write_read_roundtrip ... ok
test store::io::sync::tests::superblock_slot_write_and_fsync ... ok
test store::object::tests::index_basics ... ok
test store::root::tests::corrupt_root_rejected ... ok
test store::io::sync::tests::torn_tail_truncated_at_open ... ok
test rans::sequence::tests::shared_parse_uses_shared_dictionary_and_roundtrips_exactly ... ok
test store::index::tests::range_scan ... ok
test store::root::tests::torn_slot_ignored ... ok
test store::index::tests::replace_updates_value ... ok
test store::root::tests::generation_selection ... ok
test store::index::tests::string_keys_lexicographic ... ok
test store::segment::tests::append_flush_sync_scan ... ok
test store::segment::tests::list_and_delete ... ok
test store::segment::tests::torn_tail_ignored ... ok
test store::inode::tests::corrupt_inode_rejected ... ok
test store::snapshot::tests::snapshot_tree_ops ... ok
test store::segment::tests::corrupt_middle_detected ... ok
test store::root::tests::root_roundtrip ... ok
test rans::sequence::tests::dict_urandom_has_no_fake_density ... ok
test store::transaction::tests::crash_points_are_distinct ... ok
test tests::concurrency::group_commit_batch_composes_partial_writes ... ok
test rans::sequence::tests::shared_urandom_has_no_fake_density ... ok
test tests::base_sequence::inserted_region_roundtrips_and_uses_base_sequence ... ok
test store::index::tests::apply_sorted_batch_keeps_unchanged_subtrees ... ok
test evidence::corpus::tests::source_pack_is_deterministic ... ok
test tests::concurrency::group_commit_batch_dedup_survives_in_batch_overwrite ... ok
test store::index::tests::apply_sorted_batch_matches_sequential_ops ... ok
test rans::sequence::tests::versioned_class2_mutated_roundtrips ... ok
test tests::concurrency::group_commit_batch_dedups_within_the_batch ... ok
test tests::crash_recovery::gc_crash_at_delete_still_reclaims_later ... ok
test tests::durability::recovery_falls_back_to_newest_valid_root_record ... ok
test tests::durability::deferred_writes_survive_process_crash ... ok
test tests::durability::power_loss_keeps_only_barrier_d_data_and_never_wedges ... ok
test tests::enospc::gc_preserves_live_sequence_rans_objects ... ok
test store::index::tests::insert_get_remove_roundtrip ... ok
test tests::crash_recovery::crash_between_commits_is_linearizable ... ok
test evidence::corpus::tests::shuffled_preserves_byte_multiset ... ok
test tests::crash_recovery::commit_crash_matrix_then_fsck_and_rewrite ... ok
test tests::enospc::gc_unreachable_bytes_counts_sequence_rans ... ok
test tests::concurrency::group_commit_is_one_root ... ok
test tests::epoch::epoch_checkpoint_after_replay_is_idempotent ... ok
test tests::concurrency::parallel_writers_different_files_exact ... ok
test tests::epoch::epoch_unlink_rename_semantics ... ok
test tests::epoch::epoch_flushes_before_gc ... ok
test tests::epoch::epoch_duplicate_content_dedups_at_checkpoint ... ok
test tests::fsck::corrupt_superblock_slot_is_ignored_with_warning ... ok
test tests::fsck::clean_store_passes_fsck ... ok
test tests::epoch::epoch_crash_recovery_replays_uncheckpointed_log ... ok
test tests::fsck::deleted_root_object_is_detected ... ok
test tests::fsck::fsck_refuses_mounted_store ... ok
test tests::fsck::mid_file_corruption_is_an_error ... ok
test tests::fsck::torn_tail_is_warning_and_repairable ... ok
test tests::fsck::overwritten_data_is_reported_as_unreachable ... ok
test tests::concurrency::concurrent_fsync_and_writes ... ok
test tests::fsck::verify_materialized_full_chain ... ok
test tests::fsck::verify_materialized_with_dedup_aliases ... ok
test tests::concurrency::same_file_disjoint_region_writes ... ok
test tests::enospc::delete_then_write_works_under_pressure ... ok
test tests::hostile_media::descriptor_court::descriptor_cap_boundary ... ok
test tests::hostile_media::descriptor_court::descriptor_exhibits_pass ... ok
test tests::hostile_media::descriptor_court::seeds_bounded_under_tight_limits ... ok
test tests::hostile_media::descriptor_court::slice_oracle ... ok
test tests::hostile_media::descriptor_court::exhibits_never_panic ... ok
test tests::hostile_media::descriptor_court::trailing_garbage_oracle ... ok
test tests::hostile_media::descriptor_court::seeds_are_canonical_and_valid ... ok
test tests::hostile_media::descriptor_court::truncation_at_every_boundary_of_every_seed ... ok
test tests::hostile_media::descriptor_court::mutated_seeds_oracle ... ok
test tests::epoch::epoch_sequential_writes_form_chains_and_stay_exact ... ok
test tests::hostile_media::descriptor_court::uniform_noise_oracle ... ok
test tests::hostile_media::graph_court::graph_exhibits_pass ... ok
test tests::epoch::epoch_create_write_setattr_roundtrip_and_checkpoint ... ok
test tests::hostile_media::graph_court::graph_seeds_bounded_under_tight_limits ... ok
test tests::hostile_media::graph_court::graph_seeds_materialize_to_pinned_content ... ok
test tests::hostile_media::graph_court::mutated_graphs_oracle ... ok
test tests::hostile_media::store_court::diamond_deepest_path_chain_depth ... ok
test tests::enospc::failed_commit_leaves_no_partial_state ... ok
test tests::hostile_media::store_court::mutation_log_duplicate_and_nonmonotonic_sequences ... ok
test tests::enospc::fills_to_watermark_then_enospc ... ok
test tests::hostile_media::graph_court::noise_graphs_oracle ... ok
test tests::hostile_media::store_court::btree_fanout_and_key_exhibits ... ok
test tests::hostile_media::graph_court::spliced_graphs_oracle ... ok
test tests::hostile_media::store_court::physical_splices_are_bounded ... ok
test tests::enospc::gc_recovers_space_when_near_full ... ok
test tests::hostile_media::store_court::valid_crc_envelope_containing_malicious_descriptor ... ok
test tests::hostile_media::store_court::semantic_superblock_patch_is_bounded ... ok
test tests::crash_recovery::gc_crash_matrix_preserves_live_data ... ok
test tests::hostile_media::store_court::physical_payload_flips_are_integrity_rejected ... ok
test tests::io_backend_parity::full_workload_is_byte_identical_between_backends ... ok
test tests::io_backend_parity::read_path_identical_between_backends ... ok
test tests::hostile_media::store_court::physical_header_flips_and_truncation_are_bounded ... ok
test tests::model_bundle::model_bundle_pass_never_rewrites_noise ... ok
test tests::namespace_ops::cross_parent_rename_moves_entry ... ok
test tests::namespace_ops::directory_tree_invariants_after_rename_stress ... ok
test tests::namespace_ops::cross_parent_dir_rename_adjusts_parent_nlinks ... ok
test tests::namespace_ops::git_lock_dance_reproduces_cleanly ... ok
test tests::namespace_ops::rename_missing_source_errors ... ok
test tests::namespace_ops::rename_noop_is_successful_and_preserves_state ... ok
test tests::namespace_ops::rename_over_empty_dir_succeeds ... ok
test tests::namespace_ops::rename_rejects_type_mismatches ... ok
test tests::namespace_ops::rename_rejects_nonempty_dir_over_dir ... ok
test tests::namespace_ops::rmdir_removes_directory_inode ... ok
test tests::namespace_ops::same_parent_rename_moves_entry_exactly_once ... ok
test tests::namespace_ops::same_parent_rename_onto_hardlink_preserves_nlink ... ok
test tests::namespace_ops::same_parent_rename_over_existing_replaces_and_drops_old_inode ... ok
test tests::namespace_ops::unlink_one_hardlink_keeps_inode ... ok
test tests::namespace_ops::unlink_file_drops_inode_at_zero_links ... ok
test tests::optimizer::ablation_modes_preserve_bytes_and_differ ... ok
test tests::hostile_media::store_court::semantic_payload_rewrites_are_bounded ... ok
test tests::fuse_epoch::mounted_epoch_src_workload_roundtrips_and_survives_remount ... ok
test tests::io_backend_parity::commit_crash_points_are_byte_identical_between_backends ... ok
test tests::optimizer::chain_depth_reports_deepest_path_through_a_diamond ... ok
test tests::optimizer::chain_depth_resolves_through_the_chunk_index ... ok
test tests::optimizer::background_pass_densifies_sequential_edits ... ok
test tests::optimizer::current_persisted_bytes_counts_objects ... ok
test tests::optimizer::background_pass_preserves_exact_bytes ... ok
test tests::optimizer::background_pass_is_idempotent_and_byte_exact_after_remount ... ok
test tests::hostile_media::store_court::semantic_record_mutations_are_bounded ... ok
test tests::optimizer::cumulative_ladder_is_exact_and_monotone ... ok
test tests::optimizer::drift_workload_stays_shallow_and_exact ... ok
test tests::persistent_store::crash_matrix_at_every_durability_boundary ... ok
test tests::persistent_store::create_commit_remount_roundtrip ... ok
test tests::io_backend_parity::shared_dict_reads_identical_between_backends ... ok
test tests::persistent_store::gc_reclaims_and_preserves_live_data ... ok
test tests::persistent_store::partial_tail_chunk_never_exceeds_eof ... ok
test tests::io_backend_parity::gc_crash_points_are_byte_identical_between_backends ... ok
test tests::partial_window_read::partial_window_reads_after_checkpoint_interleave ... ok
test tests::persistent_store::rans_extent_survives_remount ... ok
test tests::persistent_store::sparse_file_holes_read_as_zeros ... ok
test tests::persistent_store::truncate_and_rewrite ... ok
test tests::optimizer::resumable_cursor_advances ... ok
test tests::persistent_store::shrinking_write_drops_extents_past_eof ... ok
test tests::persistent_store::uuid_and_features_persist ... ok
test tests::physical_convergence::compact_preserves_snapshot_roots ... ok
test tests::rank_roundtrip::multinomial_rank_unrank_roundtrip ... ok
test tests::rank_roundtrip::permutation_rank_unrank_roundtrip ... ok
test tests::representation_roundtrip::all_families_agree_on_content_id ... ok
test tests::representation_roundtrip::base_residual_roundtrip ... ok
test tests::representation_roundtrip::candidate_validation_rejects_wrong_bytes ... ok
test tests::representation_roundtrip::entropy_ref_exact_match ... ok
test tests::representation_roundtrip::entropy_ref_random_data_loses_to_raw ... ok
test tests::representation_roundtrip::fill_roundtrip ... ok
test tests::representation_roundtrip::inline_and_raw_pipeline_basics ... ok
test tests::representation_roundtrip::palette_roundtrip ... ok
test tests::representation_roundtrip::periodic_roundtrip ... ok
test tests::rank_roundtrip::combination_rank_unrank_roundtrip ... ok
test tests::representation_roundtrip::sequence_rans_roundtrip_on_text ... ok
test tests::representation_roundtrip::rans_roundtrip ... ok
test tests::representation_roundtrip::sequence_rans_skips_crypto_random ... ok
test tests::representation_roundtrip::sparse_roundtrip ... ok
test tests::representation_roundtrip::sparse_block64_roundtrip ... ok
test tests::seqdeep::deep_gate_disables_background_rewrite ... ok
test tests::concurrency::concurrent_reads_during_writes ... ok
test tests::seqdeep::deep_survives_remount_with_feature_bit ... ok
test tests::seqdeep::background_pass_rewrites_to_deep_and_roundtrips ... ok
test tests::seqdict::dictionary_depth_accounted_in_costs ... ok
test tests::seqdict::dict_correlated_second_chunk_wins_sequencedict ... ok
test tests::seqdict::background_optimizer_rebases_raw_to_sequencedict ... ok
test tests::seqdict::gc_retains_dictionary_after_source_overwrite ... ok
test tests::seqdict::in_batch_dictionary_chaining_respects_depth_cap ... ok
test tests::shared_dict::shared_dict_disabled_by_option ... ok
test tests::shared_dict::shared_dict_gc_pins_shared_after_owner_delete ... ok
test tests::epoch_self_alias::parallel_identical_writes_never_self_alias_chunk_index ... ok
test tests::shared_dict::shared_dict_pass_rewrites_family_correlated_chunks ... ok
test tests::shared_dict::shared_dict_skips_unrelated_files_and_urandom ... ok
test tests::persistent_store::gc_rebuilds_derived_chunk_index_without_history_growth ... ok
test tests::shared_dict::shared_dict_pass_is_idempotent_and_byte_exact_after_remount ... ok
test tests::snapshots::snapshot_lifecycle_create_list_delete_restore ... ok
test tests::snapshots::snapshot_pins_inodes_across_remount ... ok
test tests::sparse_block64::overflow_range_sparse_roundtrips_via_write_path ... ok
test tests::snapshots::gc_respects_snapshot_roots ... ok
test tests::snapshots::snapshot_crash_matrix_is_linearizable ... ok
test tests::seqrans_versioned_repro::versioned_corpus_roundtrips_after_each_version ... ok
test tests::srctree_diag::print_model_size_vs_scale_bits ... ok
test tests::shared_dict::anchor_pool_covers_heterogeneous_directory ... ok
test tests::srctree_diag::print_deep_vs_fast_on_pack ... ok
test tests::namespace_repro::namespace_pattern_repro ... ok
test tests::unsafe_ledger::unsafe_files_match_ledger ... ok
test tests::hostile_media::store_court::whole_store_mutator ... ok
test tests::write_parallel::committed_content_reused_in_second_file_is_byte_exact ... ok
test tests::write_parallel::consecutive_identical_chunks_in_one_batch ... ok
test tests::write_parallel::duplicate_chunks_after_aliased_first_occurrence ... ok
test tests::write_parallel::identical_content_encodes_deterministically_across_stores ... ok
test tests::write_parallel::in_batch_dict_chain_never_exceeds_decode_cap ... ok
test tests::write_parallel::parallel_multi_chunk_writes_are_byte_exact ... ok
test ublk::block::tests::block_roundtrip ... ok
test ublk::block::tests::device_is_visible_as_a_hidden_file ... ok
test ublk::block::tests::device_survives_reopen_and_fsck ... ok
test ublk::block::tests::discard_reads_zeros_and_frees ... ok
test tests::split_write::out_of_order_split_writes_compose_byte_exact ... ok
test tests::epoch_seq_monotonic::epoch_sequence_is_globally_monotonic_across_checkpoints ... ok
test tests::hostile_media::store_court::superblock_mutator ... ok
test tests::uring_bench::bench_ring_vs_pread ... ok
test tests::perf_diag::print_direct_store_perf_diag ... ok
test tests::perf_diag::foreground_policy_is_byte_exact_and_background_recovers_density ... ok
test tests::court_repro::parallel_offset_split_writes_with_setattr_flushes_stay_byte_exact ... ok
test tests::srctree_diag::print_shared_dict_ceiling ... ok
test tests::srctree_diag::print_shared_dict_pass_on_real_tree ... ok
test tests::physical_convergence::print_physical_reconciliation_real_tree ... ok
test tests::model_oracle::print_model_sharing_oracle ... ok
test tests::physical_convergence::full_compaction_converges_real_tree ... ok
test tests::srctree_diag::print_tree_gap_decomposition ... ok
test tests::model_bundle::model_bundle_pass_is_byte_exact_idempotent_and_shrinks_the_real_tree ... ok
test tests::srctree_diag::print_srctree_gate_evidence ... ok

test result: ok. 413 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 5.02s

```

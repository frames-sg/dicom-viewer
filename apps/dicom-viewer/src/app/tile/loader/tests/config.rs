use super::*;

#[test]
fn worker_count_override_supports_the_benchmark_matrix_without_changing_defaults() {
    assert_eq!(configured_worker_count(None, 12), 4);
    assert_eq!(configured_worker_count(Some("1"), 12), 1);
    assert_eq!(configured_worker_count(Some("2"), 12), 2);
    assert_eq!(configured_worker_count(Some("4"), 12), 4);
    assert_eq!(configured_worker_count(Some("invalid"), 12), 4);
}

#[test]
fn interactive_batch_size_override_accepts_only_benchmark_candidates() {
    assert_eq!(configured_interactive_batch_size(None), 2);
    assert_eq!(configured_interactive_batch_size(Some("2")), 2);
    assert_eq!(configured_interactive_batch_size(Some("4")), 4);
    assert_eq!(configured_interactive_batch_size(Some("8")), 8);
    assert_eq!(configured_interactive_batch_size(Some("1")), 2);
    assert_eq!(configured_interactive_batch_size(Some("16")), 2);
    assert_eq!(configured_interactive_batch_size(Some("invalid")), 2);
}

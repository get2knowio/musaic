pub const OBJECTS: &[(&str, &str, &str)] = &[
    ("table", "library_epoch", "library_epoch"),
    ("table", "delegated_work", "delegated_work"),
    (
        "index",
        "sqlite_autoindex_delegated_work_1",
        "delegated_work",
    ),
    (
        "index",
        "sqlite_autoindex_delegated_work_2",
        "delegated_work",
    ),
    ("index", "delegated_work_unresolved", "delegated_work"),
    ("index", "delegated_work_eligible", "delegated_work"),
    ("index", "delegated_work_groups", "delegated_work"),
    ("index", "delegated_work_leases", "delegated_work"),
    ("table", "work_group_aliases", "work_group_aliases"),
    (
        "index",
        "sqlite_autoindex_work_group_aliases_1",
        "work_group_aliases",
    ),
    ("table", "work_attempts", "work_attempts"),
    ("index", "sqlite_autoindex_work_attempts_1", "work_attempts"),
    ("table", "work_results", "work_results"),
    ("index", "sqlite_autoindex_work_results_1", "work_results"),
    ("table", "work_replays", "work_replays"),
    ("index", "sqlite_autoindex_work_replays_1", "work_replays"),
    ("table", "delegation_audit", "delegation_audit"),
    ("table", "delegation_outbox", "delegation_outbox"),
    ("table", "operations", "operations"),
    ("index", "sqlite_autoindex_operations_1", "operations"),
    ("index", "sqlite_autoindex_operations_2", "operations"),
    ("index", "operations_active", "operations"),
    ("table", "mcp_tasks", "mcp_tasks"),
    ("index", "sqlite_autoindex_mcp_tasks_1", "mcp_tasks"),
    ("index", "sqlite_autoindex_mcp_tasks_2", "mcp_tasks"),
    ("index", "mcp_tasks_retention", "mcp_tasks"),
    ("table", "mcp_task_inputs", "mcp_task_inputs"),
    (
        "index",
        "sqlite_autoindex_mcp_task_inputs_1",
        "mcp_task_inputs",
    ),
];

/// The v23 objects in `sqlite_master` order, after [`OBJECTS`].
#[allow(dead_code)] // not every test binary that includes this module uses it
pub const V23_OBJECTS: &[(&str, &str, &str)] = &[
    ("table", "delegation_events", "delegation_events"),
    ("index", "delegation_events_epoch", "delegation_events"),
    ("index", "delegation_events_age", "delegation_events"),
    ("index", "delegation_events_work", "delegation_events"),
    ("table", "delegation_event_state", "delegation_event_state"),
    (
        "table",
        "agent_host_observations",
        "agent_host_observations",
    ),
    (
        "index",
        "sqlite_autoindex_agent_host_observations_1",
        "agent_host_observations",
    ),
];

/// The objects of the Python app's schema that v24 drops: the old job table
/// and its indexes, folded into `research_history`.
#[allow(dead_code)]
pub const V24_DROPPED: &[&str] = &[
    "ai_jobs",
    "sqlite_autoindex_ai_jobs_1",
    "idx_ai_jobs_status",
    "idx_ai_jobs_album",
    "idx_ai_jobs_created",
];

/// The objects of [`OBJECTS`] that v24 rebuilds (`work_replays`, to allow
/// `claim_many`): they leave their place and come back at the end, in
/// [`V24_OBJECTS`].
#[allow(dead_code)]
pub const V24_REBUILT: &[&str] = &["work_replays", "sqlite_autoindex_work_replays_1"];

/// [`OBJECTS`] as they stand after v24 (without [`V24_REBUILT`]).
#[allow(dead_code)]
pub fn objects_v24() -> impl Iterator<Item = &'static (&'static str, &'static str, &'static str)> {
    OBJECTS.iter().filter(|o| !V24_REBUILT.contains(&o.1))
}

/// The v24 objects in `sqlite_master` order, after [`V23_OBJECTS`].
#[allow(dead_code)]
pub const V24_OBJECTS: &[(&str, &str, &str)] = &[
    ("table", "research_history", "research_history"),
    (
        "index",
        "sqlite_autoindex_research_history_1",
        "research_history",
    ),
    ("index", "research_history_album", "research_history"),
    ("index", "research_history_created", "research_history"),
    ("table", "work_batches", "work_batches"),
    ("index", "sqlite_autoindex_work_batches_1", "work_batches"),
    ("index", "work_batches_epoch", "work_batches"),
    ("index", "delegated_work_batch", "delegated_work"),
    ("table", "work_replays", "work_replays"),
    ("index", "sqlite_autoindex_work_replays_1", "work_replays"),
];

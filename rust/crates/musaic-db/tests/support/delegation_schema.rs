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

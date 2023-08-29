
// This array defines the priority order for any multi-value config
pub const MULTI_VAL_CONFIGS_PRIORITY_LIST: [&str; 2] = ["pg_stat_statements", "pg_stat_kcache"];

pub const REQUIRES_LOAD: [&str; 22] = [
    "auth_delay",
    "auto_explain",
    "basebackup_to_shell",
    "basic_archive",
    "citus",
    "passwordcheck",
    "pg_anonymize",
    "pgaudit",
    "pg_cron",
    "pg_failover_slots",
    "pg_later",
    "pglogical",
    "pg_net",
    "pg_stat_kcache",
    "pg_stat_statements",
    "pg_tle",
    "plrust",
    "postgresql_anonymizer",
    "sepgsql",
    "supautils",
    "timescaledb",
    "vectorize",
];

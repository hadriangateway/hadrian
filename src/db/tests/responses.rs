//! Shared repository tests for [`ResponsesRepo`], focused on the
//! background-worker `claim_queued` path. Run against both SQLite
//! (fast, in-memory) and PostgreSQL (testcontainers, `--ignored`).
//!
//! The Postgres variant is a regression test for the ambiguous
//! `RETURNING id` in the claim CTE (`column reference "id" is
//! ambiguous`): the error was raised at analyze time, so under the bug
//! every call below errors — including the empty-queue one.

use chrono::{DateTime, Duration, Utc};
use uuid::Uuid;

use crate::db::repos::{
    NewResponse, ResponseOwnerType, ResponseStatus, ResponsesRepo, truncate_to_millis,
};

/// Insert a response row for `org_id` with an explicit status and
/// `created_at` (claim order is oldest-first on `created_at`).
async fn seed(
    repo: &dyn ResponsesRepo,
    org_id: Uuid,
    id: &str,
    status: ResponseStatus,
    created_at: DateTime<Utc>,
) {
    repo.insert(NewResponse {
        id: id.to_string(),
        org_id,
        owner_type: ResponseOwnerType::Organization,
        owner_id: org_id,
        project_id: None,
        user_id: None,
        api_key_id: None,
        service_account_id: None,
        status,
        background: true,
        model: "test-model".to_string(),
        provider: None,
        created_at,
        request_payload: serde_json::json!({"model": "test-model"}),
        retention_expires_at: created_at + Duration::days(30),
    })
    .await
    .expect("insert response");
}

/// An empty queue yields `Ok(None)`, not an error.
pub async fn claim_queued_empty_queue_is_none(repo: &dyn ResponsesRepo, _org_id: Uuid) {
    let claimed = repo
        .claim_queued(truncate_to_millis(Utc::now()))
        .await
        .expect("claim on empty queue");
    assert!(claimed.is_none(), "nothing to claim");
}

/// Claims drain the queue oldest-first, flipping each row to
/// `in_progress` with `started_at` stamped; non-queued rows are never
/// picked up.
pub async fn claim_queued_drains_oldest_first(repo: &dyn ResponsesRepo, org_id: Uuid) {
    let now = truncate_to_millis(Utc::now());
    seed(repo, org_id, "resp_newer", ResponseStatus::Queued, now).await;
    seed(
        repo,
        org_id,
        "resp_older",
        ResponseStatus::Queued,
        now - Duration::minutes(5),
    )
    .await;
    // Already running — must not be re-claimed even though it's oldest.
    seed(
        repo,
        org_id,
        "resp_running",
        ResponseStatus::InProgress,
        now - Duration::hours(1),
    )
    .await;

    let first = repo
        .claim_queued(now)
        .await
        .expect("claim")
        .expect("queued row available");
    assert_eq!(first.id, "resp_older", "oldest queued row claimed first");
    assert_eq!(first.status, ResponseStatus::InProgress);
    assert_eq!(first.started_at, Some(now), "claim stamps started_at");
    assert_eq!(first.org_id, org_id, "RETURNING columns hydrate the record");

    let second = repo
        .claim_queued(now)
        .await
        .expect("claim")
        .expect("second queued row available");
    assert_eq!(second.id, "resp_newer");

    // Queue drained; the in_progress row is not claimable.
    assert!(
        repo.claim_queued(now).await.expect("claim").is_none(),
        "no queued rows remain"
    );
}

// ============================================================================
// SQLite Tests - Fast, in-memory
// ============================================================================

#[cfg(all(test, feature = "database-sqlite"))]
mod sqlite_tests {
    use uuid::Uuid;

    use crate::{
        db::{
            repos::OrganizationRepo,
            sqlite::{SqliteOrganizationRepo, SqliteResponsesRepo},
            tests::harness::{create_sqlite_pool, run_sqlite_migrations},
        },
        models::CreateOrganization,
    };

    async fn create_repo() -> (SqliteResponsesRepo, Uuid) {
        let pool = create_sqlite_pool().await;
        run_sqlite_migrations(&pool).await;
        let org = SqliteOrganizationRepo::new(pool.clone())
            .create(CreateOrganization {
                slug: "acme".to_string(),
                name: "Acme".to_string(),
            })
            .await
            .expect("create org");
        (SqliteResponsesRepo::new(pool), org.id)
    }

    macro_rules! sqlite_test {
        ($name:ident) => {
            #[tokio::test]
            async fn $name() {
                let (repo, org_id) = create_repo().await;
                super::$name(&repo, org_id).await;
            }
        };
    }

    sqlite_test!(claim_queued_empty_queue_is_none);
    sqlite_test!(claim_queued_drains_oldest_first);
}

// ============================================================================
// PostgreSQL Tests - Require Docker, run with `cargo test -- --ignored`
// ============================================================================

#[cfg(all(test, feature = "database-postgres"))]
mod postgres_tests {
    use uuid::Uuid;

    use crate::{
        db::{
            postgres::{PostgresOrganizationRepo, PostgresResponsesRepo},
            repos::OrganizationRepo,
            tests::harness::postgres::{create_isolated_postgres_pool, run_postgres_migrations},
        },
        models::CreateOrganization,
    };

    async fn create_repo() -> (PostgresResponsesRepo, Uuid) {
        let pool = create_isolated_postgres_pool().await;
        run_postgres_migrations(&pool).await;
        let org = PostgresOrganizationRepo::new(pool.clone(), None)
            .create(CreateOrganization {
                slug: "acme".to_string(),
                name: "Acme".to_string(),
            })
            .await
            .expect("create org");
        (PostgresResponsesRepo::new(pool, None), org.id)
    }

    macro_rules! postgres_test {
        ($name:ident) => {
            #[tokio::test]
            #[ignore = "Requires Docker - run with `cargo test -- --ignored`"]
            async fn $name() {
                let (repo, org_id) = create_repo().await;
                super::$name(&repo, org_id).await;
            }
        };
    }

    postgres_test!(claim_queued_empty_queue_is_none);
    postgres_test!(claim_queued_drains_oldest_first);
}

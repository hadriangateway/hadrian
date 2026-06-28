//! Shared repository tests for [`VideosRepo`]: org-scoping, keyset
//! pagination, update, and delete. Run against both SQLite (fast,
//! in-memory) and PostgreSQL (testcontainers, `--ignored`).

use chrono::{DateTime, Duration, Utc};
use uuid::Uuid;

use crate::db::repos::{
    NewVideo, ResponseOwnerType, VideoListOrder, VideoPatch, VideosRepo, truncate_to_millis,
};

async fn seed(
    repo: &dyn VideosRepo,
    org_id: Uuid,
    id: &str,
    status: &str,
    created_at: DateTime<Utc>,
) {
    repo.insert(NewVideo {
        id: id.to_string(),
        org_id,
        owner_type: ResponseOwnerType::Organization,
        owner_id: org_id,
        project_id: None,
        user_id: None,
        api_key_id: None,
        service_account_id: None,
        status: status.to_string(),
        model: "sora-2".to_string(),
        provider: Some("openai".to_string()),
        prompt: Some("a cat surfing".to_string()),
        size: Some("720x1280".to_string()),
        seconds: Some("8".to_string()),
        progress: Some(0),
        remixed_from_video_id: None,
        created_at,
        completed_at: None,
        expires_at: None,
        error: None,
        snapshot: serde_json::json!({
            "id": id, "object": "video", "model": "sora-2",
            "status": status, "created_at": 1
        }),
        retention_expires_at: created_at + Duration::days(7),
    })
    .await
    .expect("insert video");
}

/// A row is retrievable in its own org and invisible to other orgs.
pub async fn get_is_org_scoped(repo: &dyn VideosRepo, org_id: Uuid) {
    let now = truncate_to_millis(Utc::now());
    seed(repo, org_id, "video_a", "queued", now).await;

    let found = repo
        .get_by_id_and_org("video_a", org_id)
        .await
        .expect("get");
    assert!(found.is_some(), "row visible in its own org");
    assert_eq!(found.unwrap().provider.as_deref(), Some("openai"));

    let wrong_org = repo
        .get_by_id_and_org("video_a", Uuid::new_v4())
        .await
        .expect("get wrong org");
    assert!(wrong_org.is_none(), "cross-org read returns None");
}

/// Listing is newest-first and keyset-paginates by the `after` id.
pub async fn list_orders_newest_first_and_paginates(repo: &dyn VideosRepo, org_id: Uuid) {
    let now = truncate_to_millis(Utc::now());
    seed(
        repo,
        org_id,
        "video_old",
        "completed",
        now - Duration::minutes(2),
    )
    .await;
    seed(repo, org_id, "video_new", "completed", now).await;

    let (page1, has_more) = repo
        .list_for_owner(
            ResponseOwnerType::Organization,
            org_id,
            org_id,
            None,
            1,
            VideoListOrder::Desc,
        )
        .await
        .expect("list page 1");
    assert_eq!(page1.len(), 1);
    assert_eq!(page1[0].id, "video_new", "newest first");
    assert!(has_more, "more rows remain");

    let (page2, has_more2) = repo
        .list_for_owner(
            ResponseOwnerType::Organization,
            org_id,
            org_id,
            Some("video_new".to_string()),
            10,
            VideoListOrder::Desc,
        )
        .await
        .expect("list page 2");
    assert_eq!(page2.len(), 1);
    assert_eq!(page2[0].id, "video_old", "after cursor yields older row");
    assert!(!has_more2);
}

/// Listing is org-scoped: a row owned by the same principal in another org
/// must not appear. Mirrors the cross-org guard on `get`/`delete`. The two
/// rows share an `owner_id` (a `user` owner that spans orgs) but differ in
/// `org_id`, so only the caller-org row may surface.
pub async fn list_excludes_other_orgs(repo: &dyn VideosRepo, org_id: Uuid, other_org: Uuid) {
    let now = truncate_to_millis(Utc::now());
    let owner_id = Uuid::new_v4();

    let mk = |org: Uuid, id: &str| NewVideo {
        id: id.to_string(),
        org_id: org,
        owner_type: ResponseOwnerType::User,
        owner_id,
        project_id: None,
        user_id: None,
        api_key_id: None,
        service_account_id: None,
        status: "completed".to_string(),
        model: "sora-2".to_string(),
        provider: Some("openai".to_string()),
        prompt: None,
        size: None,
        seconds: None,
        progress: None,
        remixed_from_video_id: None,
        created_at: now,
        completed_at: None,
        expires_at: None,
        error: None,
        snapshot: serde_json::json!({ "id": id, "object": "video" }),
        retention_expires_at: now + Duration::days(7),
    };
    repo.insert(mk(org_id, "video_mine"))
        .await
        .expect("insert caller-org row");
    repo.insert(mk(other_org, "video_theirs"))
        .await
        .expect("insert other-org row");

    let (rows, _) = repo
        .list_for_owner(
            ResponseOwnerType::User,
            owner_id,
            org_id,
            None,
            10,
            VideoListOrder::Desc,
        )
        .await
        .expect("list");
    let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, ["video_mine"], "only the caller-org row is listed");

    // The `after` cursor must also stay org-scoped: paging within org_id from
    // its own row yields nothing, never the other org's row.
    let (page2, _) = repo
        .list_for_owner(
            ResponseOwnerType::User,
            owner_id,
            org_id,
            Some("video_mine".to_string()),
            10,
            VideoListOrder::Desc,
        )
        .await
        .expect("list after cursor");
    assert!(
        page2.is_empty(),
        "cursor paging stays within the caller org"
    );
}

/// Update refreshes status/snapshot; delete is org-scoped and idempotent.
pub async fn update_and_delete(repo: &dyn VideosRepo, org_id: Uuid) {
    let now = truncate_to_millis(Utc::now());
    seed(repo, org_id, "video_d", "queued", now).await;

    let updated = repo
        .update_within_org(
            "video_d",
            org_id,
            VideoPatch {
                status: "completed".to_string(),
                progress: Some(100),
                completed_at: Some(now),
                expires_at: None,
                error: None,
                snapshot: serde_json::json!({
                    "id": "video_d", "object": "video", "model": "sora-2",
                    "status": "completed", "created_at": 1
                }),
            },
        )
        .await
        .expect("update")
        .expect("row present");
    assert_eq!(updated.status, "completed");
    assert_eq!(updated.progress, Some(100));

    // Wrong-org delete is a no-op; correct-org delete removes the row.
    assert!(
        !repo
            .delete_by_id_and_org("video_d", Uuid::new_v4())
            .await
            .expect("wrong-org delete")
    );
    assert!(
        repo.delete_by_id_and_org("video_d", org_id)
            .await
            .expect("delete")
    );
    assert!(
        repo.get_by_id_and_org("video_d", org_id)
            .await
            .expect("get after delete")
            .is_none()
    );
}

#[cfg(all(test, feature = "database-sqlite"))]
mod sqlite_tests {
    use uuid::Uuid;

    use crate::{
        db::{
            repos::OrganizationRepo,
            sqlite::{SqliteOrganizationRepo, SqliteVideosRepo},
            tests::harness::{create_sqlite_pool, run_sqlite_migrations},
        },
        models::CreateOrganization,
    };

    async fn create_repo() -> (SqliteVideosRepo, Uuid) {
        let pool = create_sqlite_pool().await;
        run_sqlite_migrations(&pool).await;
        let org = SqliteOrganizationRepo::new(pool.clone())
            .create(CreateOrganization {
                slug: "acme".to_string(),
                name: "Acme".to_string(),
            })
            .await
            .expect("create org");
        (SqliteVideosRepo::new(pool), org.id)
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

    sqlite_test!(get_is_org_scoped);
    sqlite_test!(list_orders_newest_first_and_paginates);
    sqlite_test!(update_and_delete);

    #[tokio::test]
    async fn list_excludes_other_orgs() {
        let pool = create_sqlite_pool().await;
        run_sqlite_migrations(&pool).await;
        let orgs = SqliteOrganizationRepo::new(pool.clone());
        let org_a = orgs
            .create(CreateOrganization {
                slug: "acme".to_string(),
                name: "Acme".to_string(),
            })
            .await
            .expect("create org a");
        let org_b = orgs
            .create(CreateOrganization {
                slug: "globex".to_string(),
                name: "Globex".to_string(),
            })
            .await
            .expect("create org b");
        let repo = SqliteVideosRepo::new(pool);
        super::list_excludes_other_orgs(&repo, org_a.id, org_b.id).await;
    }
}

#[cfg(all(test, feature = "database-postgres"))]
mod postgres_tests {
    use uuid::Uuid;

    use crate::{
        db::{
            postgres::{PostgresOrganizationRepo, PostgresVideosRepo},
            repos::OrganizationRepo,
            tests::harness::postgres::{create_isolated_postgres_pool, run_postgres_migrations},
        },
        models::CreateOrganization,
    };

    async fn create_repo() -> (PostgresVideosRepo, Uuid) {
        let pool = create_isolated_postgres_pool().await;
        run_postgres_migrations(&pool).await;
        let org = PostgresOrganizationRepo::new(pool.clone(), None)
            .create(CreateOrganization {
                slug: "acme".to_string(),
                name: "Acme".to_string(),
            })
            .await
            .expect("create org");
        (PostgresVideosRepo::new(pool, None), org.id)
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

    postgres_test!(get_is_org_scoped);
    postgres_test!(list_orders_newest_first_and_paginates);
    postgres_test!(update_and_delete);

    #[tokio::test]
    #[ignore = "Requires Docker - run with `cargo test -- --ignored`"]
    async fn list_excludes_other_orgs() {
        let pool = create_isolated_postgres_pool().await;
        run_postgres_migrations(&pool).await;
        let orgs = PostgresOrganizationRepo::new(pool.clone(), None);
        let org_a = orgs
            .create(CreateOrganization {
                slug: "acme".to_string(),
                name: "Acme".to_string(),
            })
            .await
            .expect("create org a");
        let org_b = orgs
            .create(CreateOrganization {
                slug: "globex".to_string(),
                name: "Globex".to_string(),
            })
            .await
            .expect("create org b");
        let repo = PostgresVideosRepo::new(pool, None);
        super::list_excludes_other_orgs(&repo, org_a.id, org_b.id).await;
    }
}

#![allow(clippy::missing_errors_doc)]

/// Organization API endpoints for the Fast.io REST API.
///
/// Maps to the endpoints documented in `/current/org/`.
use std::collections::HashMap;

use serde_json::Value;

use crate::client::ApiClient;
use crate::error::CliError;

/// List the current user's organizations.
///
/// `GET /orgs/list/`
pub async fn list_orgs(
    client: &ApiClient,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<Value, CliError> {
    let mut params = HashMap::new();
    if let Some(l) = limit {
        params.insert("limit".to_owned(), l.to_string());
    }
    if let Some(o) = offset {
        params.insert("offset".to_owned(), o.to_string());
    }
    if params.is_empty() {
        client.get("/orgs/list/").await
    } else {
        client.get_with_params("/orgs/list/", &params).await
    }
}

/// Create an organization.
///
/// `POST /org/create/`
pub async fn create_org(
    client: &ApiClient,
    domain: &str,
    name: &str,
    description: Option<&str>,
    industry: Option<&str>,
    billing_email: Option<&str>,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert("domain".to_owned(), domain.to_owned());
    form.insert("name".to_owned(), name.to_owned());
    if let Some(v) = description {
        form.insert("description".to_owned(), v.to_owned());
    }
    if let Some(v) = industry {
        form.insert("industry".to_owned(), v.to_owned());
    }
    if let Some(v) = billing_email {
        form.insert("billing_email".to_owned(), v.to_owned());
    }
    client.post("/org/create/", &form).await
}

/// Get organization details.
///
/// `GET /org/{org_id}/details/`
pub async fn get_org(client: &ApiClient, org_id: &str) -> Result<Value, CliError> {
    let path = format!("/org/{}/details/", urlencoding::encode(org_id));
    client.get(&path).await
}

/// Parameters for [`update_org`].
pub struct UpdateOrgParams<'a> {
    /// Organization to update.
    pub org_id: &'a str,
    /// New display name for the organization.
    pub name: Option<&'a str>,
    /// Custom domain associated with the organization.
    pub domain: Option<&'a str>,
    /// Short description of the organization.
    pub description: Option<&'a str>,
    /// Industry vertical (e.g. `technology`, `finance`).
    pub industry: Option<&'a str>,
    /// Email address for billing notifications.
    pub billing_email: Option<&'a str>,
    /// Organization homepage URL.
    pub homepage_url: Option<&'a str>,
}

/// Update organization settings.
///
/// `POST /org/{org_id}/update/`
pub async fn update_org(
    client: &ApiClient,
    params: &UpdateOrgParams<'_>,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    if let Some(v) = params.name {
        form.insert("name".to_owned(), v.to_owned());
    }
    if let Some(v) = params.domain {
        form.insert("domain".to_owned(), v.to_owned());
    }
    if let Some(v) = params.description {
        form.insert("description".to_owned(), v.to_owned());
    }
    if let Some(v) = params.industry {
        form.insert("industry".to_owned(), v.to_owned());
    }
    if let Some(v) = params.billing_email {
        form.insert("billing_email".to_owned(), v.to_owned());
    }
    if let Some(v) = params.homepage_url {
        form.insert("homepage_url".to_owned(), v.to_owned());
    }
    let path = format!("/org/{}/update/", urlencoding::encode(params.org_id));
    client.post(&path, &form).await
}

/// Close (delete) an organization.
///
/// `POST /org/{org_id}/close/`
pub async fn close_org(client: &ApiClient, org_id: &str, confirm: &str) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert("confirm".to_owned(), confirm.to_owned());
    let path = format!("/org/{}/close/", urlencoding::encode(org_id));
    client.post(&path, &form).await
}

/// Get billing details.
///
/// `GET /org/{org_id}/billing/details/`
pub async fn get_billing_details(client: &ApiClient, org_id: &str) -> Result<Value, CliError> {
    let path = format!("/org/{}/billing/details/", urlencoding::encode(org_id));
    client.get(&path).await
}

/// List available billing plans.
///
/// `GET /org/billing/plan/list/`
pub async fn list_billing_plans(client: &ApiClient) -> Result<Value, CliError> {
    client.get("/org/billing/plan/list/").await
}

/// Get usage meters.
///
/// `GET /org/{org_id}/billing/usage/meters/list/`
pub async fn get_billing_meters(
    client: &ApiClient,
    org_id: &str,
    meter: &str,
    start_time: Option<&str>,
    end_time: Option<&str>,
) -> Result<Value, CliError> {
    let mut params = HashMap::new();
    params.insert("meter".to_owned(), meter.to_owned());
    if let Some(v) = start_time {
        params.insert("start_time".to_owned(), v.to_owned());
    }
    if let Some(v) = end_time {
        params.insert("end_time".to_owned(), v.to_owned());
    }
    let path = format!(
        "/org/{}/billing/usage/meters/list/",
        urlencoding::encode(org_id),
    );
    client.get_with_params(&path, &params).await
}

/// List organization members.
///
/// `GET /org/{org_id}/members/list/`
pub async fn list_org_members(
    client: &ApiClient,
    org_id: &str,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<Value, CliError> {
    let mut params = HashMap::new();
    if let Some(l) = limit {
        params.insert("limit".to_owned(), l.to_string());
    }
    if let Some(o) = offset {
        params.insert("offset".to_owned(), o.to_string());
    }
    let path = format!("/org/{}/members/list/", urlencoding::encode(org_id));
    if params.is_empty() {
        client.get(&path).await
    } else {
        client.get_with_params(&path, &params).await
    }
}

/// Invite a member to an organization.
///
/// `POST /org/{org_id}/members/{email}/`
pub async fn invite_org_member(
    client: &ApiClient,
    org_id: &str,
    email: &str,
    role: Option<&str>,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert(
        "permissions".to_owned(),
        role.unwrap_or("member").to_owned(),
    );
    let path = format!(
        "/org/{}/members/{}/",
        urlencoding::encode(org_id),
        urlencoding::encode(email),
    );
    client.post(&path, &form).await
}

/// Remove a member from an organization.
///
/// `DELETE /org/{org_id}/members/{member_id}/`
pub async fn remove_org_member(
    client: &ApiClient,
    org_id: &str,
    member_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/org/{}/members/{}/",
        urlencoding::encode(org_id),
        urlencoding::encode(member_id),
    );
    client.delete(&path).await
}

/// Update a member's role in an organization.
///
/// `POST /org/{org_id}/member/{member_id}/update/`
pub async fn update_org_member_role(
    client: &ApiClient,
    org_id: &str,
    member_id: &str,
    role: &str,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert("permissions".to_owned(), role.to_owned());
    let path = format!(
        "/org/{}/member/{}/update/",
        urlencoding::encode(org_id),
        urlencoding::encode(member_id),
    );
    client.post(&path, &form).await
}

/// Transfer organization ownership.
///
/// `GET /org/{org_id}/member/{user_id}/transfer_ownership/`
pub async fn transfer_org_ownership(
    client: &ApiClient,
    org_id: &str,
    user_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/org/{}/member/{}/transfer_ownership/",
        urlencoding::encode(org_id),
        urlencoding::encode(user_id),
    );
    client.get(&path).await
}

/// Discover available organizations (ones the user can join).
///
/// `GET /orgs/available/`
pub async fn discover_orgs(
    client: &ApiClient,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<Value, CliError> {
    let mut params = HashMap::new();
    if let Some(l) = limit {
        params.insert("limit".to_owned(), l.to_string());
    }
    if let Some(o) = offset {
        params.insert("offset".to_owned(), o.to_string());
    }
    if params.is_empty() {
        client.get("/orgs/available/").await
    } else {
        client.get_with_params("/orgs/available/", &params).await
    }
}

/// Get public org details (no membership required).
///
/// `GET /org/{org_id}/public/details/`
pub async fn get_public_details(client: &ApiClient, org_id: &str) -> Result<Value, CliError> {
    let path = format!("/org/{}/public/details/", urlencoding::encode(org_id));
    client.get(&path).await
}

/// Get plan limits for an org.
///
/// `GET /org/{org_id}/billing/usage/limits/credits/`
pub async fn get_limits(client: &ApiClient, org_id: &str) -> Result<Value, CliError> {
    let path = format!(
        "/org/{}/billing/usage/limits/credits/",
        urlencoding::encode(org_id),
    );
    client.get(&path).await
}

/// Get member details by user ID.
///
/// `GET /org/{org_id}/member/{user_id}/details/`
pub async fn get_member_details(
    client: &ApiClient,
    org_id: &str,
    user_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/org/{}/member/{}/details/",
        urlencoding::encode(org_id),
        urlencoding::encode(user_id),
    );
    client.get(&path).await
}

/// Leave an organization.
///
/// `DELETE /org/{org_id}/member/`
pub async fn leave_org(client: &ApiClient, org_id: &str) -> Result<Value, CliError> {
    let path = format!("/org/{}/member/", urlencoding::encode(org_id));
    client.delete(&path).await
}

/// Join an organization.
///
/// `POST /org/{org_id}/members/join/`
pub async fn join_org(client: &ApiClient, org_id: &str) -> Result<Value, CliError> {
    let form = HashMap::new();
    let path = format!("/org/{}/members/join/", urlencoding::encode(org_id));
    client.post(&path, &form).await
}

/// Cancel a billing subscription.
///
/// `DELETE /org/{org_id}/billing/`
pub async fn billing_cancel(client: &ApiClient, org_id: &str) -> Result<Value, CliError> {
    let path = format!("/org/{}/billing/", urlencoding::encode(org_id));
    client.delete(&path).await
}

/// Activate a billing subscription.
///
/// `POST /org/{org_id}/billing/activate/`
pub async fn billing_activate(client: &ApiClient, org_id: &str) -> Result<Value, CliError> {
    let form = HashMap::new();
    let path = format!("/org/{}/billing/activate/", urlencoding::encode(org_id));
    client.post(&path, &form).await
}

/// Reset billing.
///
/// `POST /org/{org_id}/billing/reset/`
pub async fn billing_reset(client: &ApiClient, org_id: &str) -> Result<Value, CliError> {
    let form = HashMap::new();
    let path = format!("/org/{}/billing/reset/", urlencoding::encode(org_id));
    client.post(&path, &form).await
}

/// List billable members.
///
/// `GET /org/{org_id}/billing/usage/members/list/`
pub async fn billing_members(
    client: &ApiClient,
    org_id: &str,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<Value, CliError> {
    let mut params = HashMap::new();
    if let Some(l) = limit {
        params.insert("limit".to_owned(), l.to_string());
    }
    if let Some(o) = offset {
        params.insert("offset".to_owned(), o.to_string());
    }
    let path = format!(
        "/org/{}/billing/usage/members/list/",
        urlencoding::encode(org_id),
    );
    if params.is_empty() {
        client.get(&path).await
    } else {
        client.get_with_params(&path, &params).await
    }
}

/// Create a billing subscription.
///
/// `POST /org/{org_id}/billing/`
pub async fn billing_create(
    client: &ApiClient,
    org_id: &str,
    plan_id: Option<&str>,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    if let Some(p) = plan_id {
        form.insert("billing_plan".to_owned(), p.to_owned());
    }
    let path = format!("/org/{}/billing/", urlencoding::encode(org_id));
    client.post(&path, &form).await
}

/// List org invitations.
///
/// `GET /org/{org_id}/members/invitations/list/`
pub async fn list_invitations(
    client: &ApiClient,
    org_id: &str,
    state: Option<&str>,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<Value, CliError> {
    let path = if let Some(s) = state {
        format!(
            "/org/{}/members/invitations/list/{}/",
            urlencoding::encode(org_id),
            urlencoding::encode(s),
        )
    } else {
        format!(
            "/org/{}/members/invitations/list/",
            urlencoding::encode(org_id),
        )
    };
    let mut params = HashMap::new();
    if let Some(l) = limit {
        params.insert("limit".to_owned(), l.to_string());
    }
    if let Some(o) = offset {
        params.insert("offset".to_owned(), o.to_string());
    }
    if params.is_empty() {
        client.get(&path).await
    } else {
        client.get_with_params(&path, &params).await
    }
}

/// Update an org invitation.
///
/// `POST /org/{org_id}/members/invitation/{invitation_id}/`
pub async fn update_invitation(
    client: &ApiClient,
    org_id: &str,
    invitation_id: &str,
    state: Option<&str>,
    role: Option<&str>,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    if let Some(s) = state {
        form.insert("state".to_owned(), s.to_owned());
    }
    if let Some(r) = role {
        form.insert("permissions".to_owned(), r.to_owned());
    }
    let path = format!(
        "/org/{}/members/invitation/{}/",
        urlencoding::encode(org_id),
        urlencoding::encode(invitation_id),
    );
    client.post(&path, &form).await
}

/// Delete an org invitation.
///
/// `DELETE /org/{org_id}/members/invitation/{invitation_id}/`
pub async fn delete_invitation(
    client: &ApiClient,
    org_id: &str,
    invitation_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/org/{}/members/invitation/{}/",
        urlencoding::encode(org_id),
        urlencoding::encode(invitation_id),
    );
    client.delete(&path).await
}

/// Create a transfer token.
///
/// `POST /org/{org_id}/transfer/token/create/`
pub async fn transfer_token_create(client: &ApiClient, org_id: &str) -> Result<Value, CliError> {
    let form = HashMap::new();
    let path = format!(
        "/org/{}/transfer/token/create/",
        urlencoding::encode(org_id),
    );
    client.post(&path, &form).await
}

/// List transfer tokens.
///
/// `GET /org/{org_id}/transfer/token/list/`
pub async fn transfer_token_list(
    client: &ApiClient,
    org_id: &str,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<Value, CliError> {
    let path = format!("/org/{}/transfer/token/list/", urlencoding::encode(org_id),);
    let mut params = HashMap::new();
    if let Some(l) = limit {
        params.insert("limit".to_owned(), l.to_string());
    }
    if let Some(o) = offset {
        params.insert("offset".to_owned(), o.to_string());
    }
    if params.is_empty() {
        client.get(&path).await
    } else {
        client.get_with_params(&path, &params).await
    }
}

/// Delete a transfer token.
///
/// `DELETE /org/{org_id}/transfer/token/{token_id}/`
pub async fn transfer_token_delete(
    client: &ApiClient,
    org_id: &str,
    token_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/org/{}/transfer/token/{}/",
        urlencoding::encode(org_id),
        urlencoding::encode(token_id),
    );
    client.delete(&path).await
}

/// Claim org ownership via transfer token.
///
/// `POST /org/transfer/claim/`
pub async fn transfer_claim(client: &ApiClient, token: &str) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert("token".to_owned(), token.to_owned());
    client.post("/org/transfer/claim/", &form).await
}

/// Discover all organizations.
///
/// `GET /orgs/all/`
pub async fn discover_all(
    client: &ApiClient,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<Value, CliError> {
    let mut params = HashMap::new();
    if let Some(l) = limit {
        params.insert("limit".to_owned(), l.to_string());
    }
    if let Some(o) = offset {
        params.insert("offset".to_owned(), o.to_string());
    }
    if params.is_empty() {
        client.get("/orgs/all/").await
    } else {
        client.get_with_params("/orgs/all/", &params).await
    }
}

/// Check domain availability.
///
/// `GET /orgs/check/domain/{domain}/`
pub async fn discover_check_domain(client: &ApiClient, domain: &str) -> Result<Value, CliError> {
    let path = format!("/orgs/check/domain/{}/", urlencoding::encode(domain),);
    client.get(&path).await
}

/// List external organizations.
///
/// `GET /orgs/list/external/`
pub async fn discover_external(
    client: &ApiClient,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<Value, CliError> {
    let mut params = HashMap::new();
    if let Some(l) = limit {
        params.insert("limit".to_owned(), l.to_string());
    }
    if let Some(o) = offset {
        params.insert("offset".to_owned(), o.to_string());
    }
    if params.is_empty() {
        client.get("/orgs/list/external/").await
    } else {
        client
            .get_with_params("/orgs/list/external/", &params)
            .await
    }
}

/// List workspaces in an org.
///
/// `GET /org/{org_id}/list/workspaces/`
pub async fn list_workspaces(
    client: &ApiClient,
    org_id: &str,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<Value, CliError> {
    let mut params = HashMap::new();
    if let Some(l) = limit {
        params.insert("limit".to_owned(), l.to_string());
    }
    if let Some(o) = offset {
        params.insert("offset".to_owned(), o.to_string());
    }
    let path = format!("/org/{}/list/workspaces/", urlencoding::encode(org_id));
    if params.is_empty() {
        client.get(&path).await
    } else {
        client.get_with_params(&path, &params).await
    }
}

/// List shares in an org.
///
/// `GET /shares/all/` (filtered by org)
pub async fn list_org_shares(
    client: &ApiClient,
    org_id: &str,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<Value, CliError> {
    let mut params = HashMap::new();
    params.insert("org_id".to_owned(), org_id.to_owned());
    if let Some(l) = limit {
        params.insert("limit".to_owned(), l.to_string());
    }
    if let Some(o) = offset {
        params.insert("offset".to_owned(), o.to_string());
    }
    client.get_with_params("/shares/all/", &params).await
}

/// Get org asset types.
///
/// `GET /org/assets/`
pub async fn org_asset_types(client: &ApiClient) -> Result<Value, CliError> {
    client.get("/org/assets/").await
}

/// List org assets.
///
/// `GET /org/{org_id}/assets/`
pub async fn list_org_assets(client: &ApiClient, org_id: &str) -> Result<Value, CliError> {
    let path = format!("/org/{}/assets/", urlencoding::encode(org_id));
    client.get(&path).await
}

/// Delete an org asset.
///
/// `DELETE /org/{org_id}/assets/{asset_name}/`
pub async fn delete_org_asset(
    client: &ApiClient,
    org_id: &str,
    asset_name: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/org/{}/assets/{}/",
        urlencoding::encode(org_id),
        urlencoding::encode(asset_name),
    );
    client.delete(&path).await
}

/// Create a workspace in an org.
///
/// `POST /workspace/create/`
pub async fn create_workspace(
    client: &ApiClient,
    org_id: &str,
    folder_name: &str,
    name: &str,
    description: Option<&str>,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert("org_id".to_owned(), org_id.to_owned());
    form.insert("folder_name".to_owned(), folder_name.to_owned());
    form.insert("name".to_owned(), name.to_owned());
    if let Some(d) = description {
        form.insert("description".to_owned(), d.to_owned());
    }
    client.post("/workspace/create/", &form).await
}

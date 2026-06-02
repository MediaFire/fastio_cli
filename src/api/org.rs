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

/// Build the path for the billing-details endpoint.
fn billing_details_path(org_id: &str) -> String {
    format!("/org/{}/billing/details/", urlencoding::encode(org_id))
}

/// Get billing details.
///
/// `GET /org/{org_id}/billing/details/`
pub async fn get_billing_details(client: &ApiClient, org_id: &str) -> Result<Value, CliError> {
    client.get(&billing_details_path(org_id)).await
}

/// List available billing plans.
///
/// `GET /org/billing/plan/list/`
pub async fn list_billing_plans(client: &ApiClient) -> Result<Value, CliError> {
    client.get("/org/billing/plan/list/").await
}

/// Parameters for [`get_billing_meters`].
pub struct BillingMetersParams<'a> {
    /// Organization to query.
    pub org_id: &'a str,
    /// Meter type (e.g. `storage_bytes`, `bandwidth_bytes`, `ai_tokens`).
    pub meter: &'a str,
    /// Start of the time range (`YYYY-MM-DD HH:MM:SS`). Defaults to 30 days ago.
    pub start_time: Option<&'a str>,
    /// End of the time range (`YYYY-MM-DD HH:MM:SS`). Defaults to now.
    pub end_time: Option<&'a str>,
    /// Filter by workspace (19-digit ID). Mutually exclusive with `share_id`.
    pub workspace_id: Option<&'a str>,
    /// Filter by share (19-digit ID). Mutually exclusive with `workspace_id`.
    pub share_id: Option<&'a str>,
}

/// Build the path for the usage-meters endpoint.
fn billing_meters_path(org_id: &str) -> String {
    format!(
        "/org/{}/billing/usage/meters/list/",
        urlencoding::encode(org_id),
    )
}

/// Build the query map for the usage-meters endpoint, enforcing the
/// `workspace_id` / `share_id` XOR BEFORE any HTTP request is issued.
///
/// Supplying both filters returns a clear [`CliError`] (the server would
/// otherwise reject it with `1605`).
fn billing_meters_query(
    params: &BillingMetersParams<'_>,
) -> Result<HashMap<String, String>, CliError> {
    if params.workspace_id.is_some() && params.share_id.is_some() {
        return Err(CliError::Parse(
            "only one of --workspace-id or --share-id may be specified".to_owned(),
        ));
    }
    let mut query = HashMap::new();
    query.insert("meter".to_owned(), params.meter.to_owned());
    if let Some(v) = params.start_time {
        query.insert("start_time".to_owned(), v.to_owned());
    }
    if let Some(v) = params.end_time {
        query.insert("end_time".to_owned(), v.to_owned());
    }
    if let Some(v) = params.workspace_id {
        query.insert("workspace_id".to_owned(), v.to_owned());
    }
    if let Some(v) = params.share_id {
        query.insert("share_id".to_owned(), v.to_owned());
    }
    Ok(query)
}

/// Get usage meters.
///
/// `GET /org/{org_id}/billing/usage/meters/list/`
///
/// `workspace_id` and `share_id` are mutually exclusive; supplying both is
/// rejected with a clear [`CliError`] BEFORE any HTTP request is issued (the
/// server would otherwise reject it with `1605`).
pub async fn get_billing_meters(
    client: &ApiClient,
    params: &BillingMetersParams<'_>,
) -> Result<Value, CliError> {
    let query = billing_meters_query(params)?;
    let path = billing_meters_path(params.org_id);
    client.get_with_params(&path, &query).await
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

/// Build the path for the credit-usage endpoint.
fn billing_credit_usage_path(org_id: &str) -> String {
    format!(
        "/org/{}/billing/usage/limits/credits/",
        urlencoding::encode(org_id),
    )
}

/// Get credit usage and limits for an org.
///
/// `GET /org/{org_id}/billing/usage/limits/credits/`
///
/// Returns the org's per-period credit consumption, remaining budget, and
/// renewal window. Reached by both `org billing usage` (canonical) and the
/// hidden `org limits` alias via [`get_limits`].
pub async fn get_credit_usage(client: &ApiClient, org_id: &str) -> Result<Value, CliError> {
    client.get(&billing_credit_usage_path(org_id)).await
}

/// Get plan limits for an org (hidden `org limits` alias).
///
/// `GET /org/{org_id}/billing/usage/limits/credits/`
///
/// Thin alias for [`get_credit_usage`], retained so the deprecated
/// `org limits` command keeps reaching the same endpoint for one release.
pub async fn get_limits(client: &ApiClient, org_id: &str) -> Result<Value, CliError> {
    get_credit_usage(client, org_id).await
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

/// Build the root billing path (`/org/{org_id}/billing/`).
///
/// Shared by the subscription create (`POST`), cancel (`DELETE`), and
/// reactivate (`PUT`) calls — the three verbs the server multiplexes on this
/// single path.
fn billing_root_path(org_id: &str) -> String {
    format!("/org/{}/billing/", urlencoding::encode(org_id))
}

/// Cancel a billing subscription.
///
/// `DELETE /org/{org_id}/billing/`
///
/// Schedules cancellation at the end of the current billing period — the org
/// keeps full access until `cancel_at`. Use [`billing_reactivate`] to reverse
/// the schedule before it executes.
pub async fn billing_cancel(client: &ApiClient, org_id: &str) -> Result<Value, CliError> {
    client.delete(&billing_root_path(org_id)).await
}

/// Reactivate a subscription scheduled to cancel at period end.
///
/// `PUT /org/{org_id}/billing/`
///
/// Owner-only on the server. Clears `cancel_at_period_end` so the
/// subscription renews normally. Calling this on a subscription that is not
/// scheduled to cancel is a successful no-op; once the subscription has fully
/// terminated the server returns `1683`/404 (use [`billing_create`] to start a
/// new subscription instead). Replaces the removed `activate`/`reset` calls,
/// whose endpoints do not exist.
pub async fn billing_reactivate(client: &ApiClient, org_id: &str) -> Result<Value, CliError> {
    // The reactivate endpoint takes no request body; send an empty JSON object
    // via the shared `put_json` helper.
    client
        .put_json(
            &billing_root_path(org_id),
            &Value::Object(serde_json::Map::new()),
        )
        .await
}

/// Build the path for the billable-members endpoint.
fn billing_members_path(org_id: &str) -> String {
    format!(
        "/org/{}/billing/usage/members/list/",
        urlencoding::encode(org_id),
    )
}

/// List billable members.
///
/// `GET /org/{org_id}/billing/usage/members/list/`
///
/// Offset-paginated (unlike the cursor-paginated invoices endpoint).
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
    let path = billing_members_path(org_id);
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
    client.post(&billing_root_path(org_id), &form).await
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

/// Build the path for the invoices endpoint.
fn billing_invoices_path(org_id: &str) -> String {
    format!("/org/{}/billing/invoices/", urlencoding::encode(org_id))
}

/// Build the cursor-pagination query for the invoices endpoint.
///
/// Emits `limit` and/or `starting_after` (the invoice-ID cursor) — never
/// `offset`. Returns an empty map when neither is supplied.
fn billing_invoices_query(
    limit: Option<u32>,
    starting_after: Option<&str>,
) -> HashMap<String, String> {
    let mut params = HashMap::new();
    if let Some(l) = limit {
        params.insert("limit".to_owned(), l.to_string());
    }
    if let Some(cursor) = starting_after {
        params.insert("starting_after".to_owned(), cursor.to_owned());
    }
    params
}

/// List billing invoices.
///
/// `GET /org/{org_id}/billing/invoices/`
///
/// Cursor-paginated: pass the `id` of the last invoice from the previous page
/// as `starting_after` to fetch the next page (NOT offset-based). The response
/// carries `has_more` to indicate whether further pages exist.
pub async fn billing_invoices(
    client: &ApiClient,
    org_id: &str,
    limit: Option<u32>,
    starting_after: Option<&str>,
) -> Result<Value, CliError> {
    let params = billing_invoices_query(limit, starting_after);
    let path = billing_invoices_path(org_id);
    if params.is_empty() {
        client.get(&path).await
    } else {
        client.get_with_params(&path, &params).await
    }
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

#[cfg(test)]
mod tests {
    use super::{
        BillingMetersParams, billing_credit_usage_path, billing_details_path,
        billing_invoices_path, billing_invoices_query, billing_members_path, billing_meters_path,
        billing_meters_query, billing_root_path,
    };
    use crate::error::CliError;

    // ─── path builders ──────────────────────────────────────────────────────

    #[test]
    fn billing_root_path_is_canonical() {
        // The PUT (reactivate), DELETE (cancel), and POST (subscribe) all hit
        // this exact path. `put_json` then sends it via the PUT verb (covered
        // by client.rs::put_json_uses_put_method_and_url).
        assert_eq!(
            billing_root_path("1234567890123456789"),
            "/org/1234567890123456789/billing/"
        );
    }

    #[test]
    fn billing_root_path_url_encodes_id() {
        assert_eq!(billing_root_path("a/b c"), "/org/a%2Fb%20c/billing/");
    }

    #[test]
    fn billing_details_path_builds() {
        assert_eq!(billing_details_path("19"), "/org/19/billing/details/");
    }

    #[test]
    fn billing_credit_usage_path_builds() {
        // Reached by both `org billing usage` and the `org limits` alias.
        assert_eq!(
            billing_credit_usage_path("19"),
            "/org/19/billing/usage/limits/credits/"
        );
    }

    #[test]
    fn billing_meters_path_builds() {
        assert_eq!(
            billing_meters_path("19"),
            "/org/19/billing/usage/meters/list/"
        );
    }

    #[test]
    fn billing_members_path_builds() {
        assert_eq!(
            billing_members_path("19"),
            "/org/19/billing/usage/members/list/"
        );
    }

    #[test]
    fn billing_invoices_path_builds() {
        assert_eq!(billing_invoices_path("19"), "/org/19/billing/invoices/");
    }

    // ─── invoices cursor pagination (NOT offset) ────────────────────────────

    #[test]
    fn invoices_query_uses_limit_and_starting_after_not_offset() {
        let q = billing_invoices_query(Some(25), Some("in_abc"));
        assert_eq!(q.get("limit").map(String::as_str), Some("25"));
        assert_eq!(q.get("starting_after").map(String::as_str), Some("in_abc"));
        assert!(
            !q.contains_key("offset"),
            "invoices must paginate by cursor, never offset"
        );
    }

    #[test]
    fn invoices_query_empty_when_no_args() {
        assert!(billing_invoices_query(None, None).is_empty());
    }

    #[test]
    fn invoices_query_cursor_only() {
        let q = billing_invoices_query(None, Some("in_last"));
        assert_eq!(q.get("starting_after").map(String::as_str), Some("in_last"));
        assert!(!q.contains_key("limit"));
    }

    // ─── meters XOR validation (before any HTTP) ────────────────────────────

    #[test]
    fn meters_query_rejects_both_filters() {
        let params = BillingMetersParams {
            org_id: "19",
            meter: "storage_bytes",
            start_time: None,
            end_time: None,
            workspace_id: Some("ws1"),
            share_id: Some("sh1"),
        };
        let err = billing_meters_query(&params).unwrap_err();
        assert!(
            matches!(err, CliError::Parse(_)),
            "both filters must be rejected before the HTTP call, got {err:?}"
        );
    }

    #[test]
    fn meters_query_accepts_workspace_only() {
        let params = BillingMetersParams {
            org_id: "19",
            meter: "storage_bytes",
            start_time: Some("2024-01-01 00:00:00"),
            end_time: Some("2024-01-31 23:59:59"),
            workspace_id: Some("ws1"),
            share_id: None,
        };
        let q = billing_meters_query(&params).expect("workspace-only is valid");
        assert_eq!(q.get("meter").map(String::as_str), Some("storage_bytes"));
        assert_eq!(q.get("workspace_id").map(String::as_str), Some("ws1"));
        assert_eq!(
            q.get("start_time").map(String::as_str),
            Some("2024-01-01 00:00:00")
        );
        assert!(!q.contains_key("share_id"));
    }

    #[test]
    fn meters_query_accepts_share_only() {
        let params = BillingMetersParams {
            org_id: "19",
            meter: "ai_tokens",
            start_time: None,
            end_time: None,
            workspace_id: None,
            share_id: Some("sh1"),
        };
        let q = billing_meters_query(&params).expect("share-only is valid");
        assert_eq!(q.get("share_id").map(String::as_str), Some("sh1"));
        assert!(!q.contains_key("workspace_id"));
    }

    #[test]
    fn meters_query_accepts_neither_filter() {
        let params = BillingMetersParams {
            org_id: "19",
            meter: "bandwidth_bytes",
            start_time: None,
            end_time: None,
            workspace_id: None,
            share_id: None,
        };
        let q = billing_meters_query(&params).expect("no filter is valid");
        assert_eq!(q.get("meter").map(String::as_str), Some("bandwidth_bytes"));
        assert!(!q.contains_key("workspace_id"));
        assert!(!q.contains_key("share_id"));
    }
}

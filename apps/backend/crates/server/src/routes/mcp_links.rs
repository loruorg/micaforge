//! MCP tools for short links: `create_link`, `update_link` and `delete_link`.
//!
//! The reads (`list_links`, `get_link_stats`) are in
//! [`super::mcp_monitoring`]; these are the writes. An agent writing a launch
//! post can make the short links it cites, with their UTM tags, and fix a
//! target after a page moved. Each tool goes through the same write path as
//! the link routes ([`crate::routes::links::create_in`] and its siblings):
//! the same slug, address and UTM checks, the same folder check, the same
//! audit entry.
//!
//! The tools act on one site's links, the ones `list_links` shows; an
//! organisation-wide link (one with no site) is managed in the dashboard.
//! Writing needs `manage:write` on the credential and `member` on the site,
//! exactly what `POST /api/links` needs. `delete_link` also needs
//! `confirm: true`: a deleted link answers `404` to everyone who still has it,
//! in print, in a QR code or in an old email.

use micaforge_core::{Error, Result};
use micaforge_store::models::links::{self as link_store, Link};
use micaforge_store::models::sites::Site;
use serde_json::{json, Map, Value};
use uuid::Uuid;

use super::mcp::{acting_user, authorize_site, require_write, site_argument, string_argument};
use super::mcp_flags::{only_known, require_confirm};
use crate::extract::AuthUser;
use crate::routes::links::{self, LinkBody};
use crate::routes::orgs::{require_at_least, MEMBER};
use crate::routes::sites::effective_site_role;
use crate::state::AppState;

/// The fields a create or an update may set, as the link routes name them.
const FIELDS: &[&str] = &[
    "slug",
    "target_url",
    "folder_id",
    "utm",
    "title",
    "description",
    "image_url",
    "ios_url",
    "android_url",
    "deep_link",
    "expires_at",
];

/// The JSON Schema of the link fields.
fn field_schemas() -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("slug".into(), json!({ "type": "string", "maxLength": 64, "description": "The short path: letters, digits, `-` or `_`, unique on this server. Left out on create, a random seven-character one is chosen." }));
    m.insert("target_url".into(), json!({ "type": "string", "maxLength": 2048, "description": "Where the link sends people: an http or https address." }));
    m.insert("folder_id".into(), json!({ "type": ["string", "null"], "description": "A link folder of the site's organisation, or null for none." }));
    m.insert("utm".into(), json!({
        "type": "object",
        "description": "UTM tags added to the target: any of source, medium, campaign, term, content. For example {\"source\":\"newsletter\",\"campaign\":\"launch\"}.",
        "properties": {
            "source": { "type": "string" }, "medium": { "type": "string" }, "campaign": { "type": "string" },
            "term": { "type": "string" }, "content": { "type": "string" }
        },
        "additionalProperties": false
    }));
    m.insert("title".into(), json!({ "type": ["string", "null"], "maxLength": 200, "description": "The preview card's title in chat apps and social posts." }));
    m.insert("description".into(), json!({ "type": ["string", "null"], "maxLength": 500, "description": "The preview card's text." }));
    m.insert("image_url".into(), json!({ "type": ["string", "null"], "description": "The preview card's image: an https address." }));
    m.insert("ios_url".into(), json!({ "type": ["string", "null"], "description": "Where iPhones and iPads go instead: http, https or itms-apps." }));
    m.insert("android_url".into(), json!({ "type": ["string", "null"], "description": "Where Android phones go instead: http, https or market." }));
    m.insert("deep_link".into(), json!({ "type": ["string", "null"], "description": "An app address tried first on a phone, such as myapp://item/42." }));
    m.insert("expires_at".into(), json!({ "type": ["string", "null"], "description": "ISO-8601 time after which the link stops redirecting and shows an expired page. Null for never." }));
    m
}

/// A tool's input schema: `site`, then `extra`.
fn schema(extra: Map<String, Value>, required: &[&str]) -> Value {
    let mut props = Map::new();
    props.insert(
        "site".into(),
        json!({ "type": "integer", "minimum": 1, "maximum": 65535, "description": "Numeric site id, from `list_sites`." }),
    );
    props.extend(extra);
    json!({
        "type": "object",
        "properties": Value::Object(props),
        "required": required,
        "additionalProperties": false
    })
}

/// The `link` argument's schema.
fn link_property() -> Value {
    json!({ "type": "string", "description": "The link's id, from `list_links`." })
}

/// This module's tools.
pub fn tools() -> Vec<Value> {
    let mut update = field_schemas();
    update.insert("link".into(), link_property());
    let mut delete = Map::new();
    delete.insert("link".into(), link_property());
    delete.insert(
        "confirm".into(),
        json!({ "type": "boolean", "const": true, "description": "Must be true. Set it only after the person agreed to delete this link." }),
    );
    vec![
        json!({
            "name": "create_link",
            "description": "Create a short link on a site: a short address that redirects to `target_url`, counts clicks \
        without cookies, and shows a preview card to chat apps. Add `utm` tags to see the link's visits as a campaign. Returns \
        the short address to use. Create a link only when the person asked for one or the text you are writing needs it. Needs \
        the manage:write scope and a member's role.",
            "inputSchema": schema(field_schemas(), &["site", "target_url"]),
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false }
        }),
        json!({
            "name": "update_link",
            "description": "Change a short link of a site, by the id `list_links` returns. Send only the fields to change; \
        null clears an optional field. Changing `slug` changes the short address, and the old one stops working. Needs the \
        manage:write scope and a member's role.",
            "inputSchema": schema(update, &["site", "link"]),
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false }
        }),
        json!({
            "name": "delete_link",
            "description": "Delete a short link for good. Everyone who still has the address (in print, a QR code, an old \
        email) gets a not-found page from then on; its click history stays in the reports. To stop a link without deleting it, \
        set `expires_at` with update_link. Ask the person before deleting, then call with `confirm: true`; without it the call is \
        refused. Needs the manage:write scope and a member's role.",
            "inputSchema": schema(delete, &["site", "link", "confirm"]),
            "annotations": { "readOnlyHint": false, "destructiveHint": true, "idempotentHint": true, "openWorldHint": false }
        }),
    ]
}

/// Run `name` if it is this module's.
pub async fn call(
    st: &AppState,
    auth: &AuthUser,
    name: &str,
    args: &Value,
) -> Option<Result<Value>> {
    let done = match name {
        "create_link" => create(st, auth, args).await,
        "update_link" => update(st, auth, args).await,
        "delete_link" => delete(st, auth, args).await,
        _ => return None,
    };
    Some(done)
}

/// A link body from the arguments laid over `base` (the link as it is, for an
/// update; nothing, for a create). A field sent as `null` is cleared.
fn link_body(args: &Value, base: Value, for_update: bool) -> Result<LinkBody> {
    let mut allowed: Vec<&str> = vec!["site"];
    if for_update {
        allowed.push("link");
    }
    allowed.extend_from_slice(FIELDS);
    only_known(args, &allowed)?;

    let mut body = match base {
        Value::Object(map) => map,
        _ => Map::new(),
    };
    for field in FIELDS {
        if let Some(v) = args.get(*field) {
            body.insert((*field).to_string(), v.clone());
        }
    }
    if body.get("target_url").is_none_or(Value::is_null) {
        return Err(Error::validation("target_url", "is required"));
    }
    serde_json::from_value(Value::Object(body))
        .map_err(|e| Error::validation("arguments", format!("could not be read as a link: {e}")))
}

/// The site, with `member` asserted on it for the calling credential.
async fn writable_site(st: &AppState, auth: &AuthUser, args: &Value) -> Result<Site> {
    let site = authorize_site(st, auth, site_argument(args)?).await?;
    let role = effective_site_role(&st.pg, &site, acting_user(auth)).await?;
    require_at_least(&role, MEMBER)?;
    Ok(site)
}

/// The link the `link` argument names, which must be one of `site`'s.
async fn site_link(st: &AppState, site: &Site, args: &Value) -> Result<Link> {
    let id: Uuid = string_argument(args, "link")
        .ok_or_else(|| Error::validation("link", "is required; call `list_links` first"))?
        .parse()
        .map_err(|_| Error::validation("link", "is not a link id"))?;
    let found = match link_store::get(&st.pg, id).await {
        Ok(link) => link,
        Err(Error::NotFound(_)) => return Err(Error::not_found("link")),
        Err(e) => return Err(e),
    };
    if found.site_id != Some(site.id) {
        return Err(Error::not_found("link"));
    }
    Ok(found)
}

/// A link answer, with the numeric site beside it.
fn answer(mut link: Value, site: &Site) -> Value {
    link["site"] = json!(site.site_id_u16());
    link
}

async fn create(st: &AppState, auth: &AuthUser, args: &Value) -> Result<Value> {
    require_write(auth)?;
    let body = link_body(args, Value::Null, false)?;
    let site = writable_site(st, auth, args).await?;
    Ok(answer(
        links::create_in(st, auth, site.org_id, Some(&site), &body).await?,
        &site,
    ))
}

async fn update(st: &AppState, auth: &AuthUser, args: &Value) -> Result<Value> {
    require_write(auth)?;
    // Checked before anything is read, then again over the stored link.
    let allowed: Vec<&str> = ["site", "link"].iter().chain(FIELDS).copied().collect();
    only_known(args, &allowed)?;
    let site = writable_site(st, auth, args).await?;
    let link = site_link(st, &site, args).await?;
    let body = link_body(args, links::editable(&link), true)?;
    Ok(answer(
        links::update_in(st, auth, &link, &body).await?,
        &site,
    ))
}

async fn delete(st: &AppState, auth: &AuthUser, args: &Value) -> Result<Value> {
    require_write(auth)?;
    only_known(args, &["site", "link", "confirm"])?;
    require_confirm(args, "a short link")?;
    let site = writable_site(st, auth, args).await?;
    let link = site_link(st, &site, args).await?;
    let mut done = links::delete_in(st, auth, &link).await?;
    done["slug"] = json!(link.slug);
    Ok(answer(done, &site))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_describes_itself_and_requires_what_it_needs() {
        let tools = tools();
        let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        assert_eq!(names, ["create_link", "update_link", "delete_link"]);
        for tool in &tools {
            assert!(tool["description"].as_str().is_some_and(|d| d.len() > 80));
            assert!(tool["inputSchema"]["required"]
                .as_array()
                .unwrap()
                .contains(&json!("site")));
        }
        let delete = &tools[2];
        assert!(delete["inputSchema"]["required"]
            .as_array()
            .unwrap()
            .contains(&json!("confirm")));
        assert_eq!(delete["annotations"]["destructiveHint"], json!(true));
    }

    #[test]
    fn a_create_needs_a_target_and_known_arguments() {
        assert_eq!(
            link_body(&json!({ "site": 1 }), Value::Null, false)
                .unwrap_err()
                .field(),
            Some("target_url")
        );
        assert_eq!(
            link_body(
                &json!({ "site": 1, "target_url": "https://example.com", "url": "x" }),
                Value::Null,
                false
            )
            .unwrap_err()
            .field(),
            Some("url")
        );
        // `link` names the link to change; it is not a create argument.
        assert!(link_body(
            &json!({ "site": 1, "target_url": "https://example.com", "link": "x" }),
            Value::Null,
            false
        )
        .is_err());
        assert!(link_body(
            &json!({ "site": 1, "target_url": "https://example.com", "utm": { "source": "x" } }),
            Value::Null,
            false
        )
        .is_ok());
        // A wrongly typed field is refused before anything is written.
        assert_eq!(
            link_body(
                &json!({ "site": 1, "target_url": "https://example.com", "expires_at": "soon" }),
                Value::Null,
                false
            )
            .unwrap_err()
            .field(),
            Some("arguments")
        );
    }

    #[test]
    fn an_update_keeps_what_it_was_not_sent_and_clears_what_was_sent_as_null() {
        let base = json!({
            "slug": "launch",
            "target_url": "https://example.com/launch",
            "title": "Launch",
            "utm": { "source": "x" }
        });
        let body = link_body(
            &json!({ "site": 1, "link": "id", "title": null, "target_url": "https://example.com/new" }),
            base.clone(),
            true,
        )
        .unwrap();
        let debug = format!("{body:?}");
        assert!(debug.contains("https://example.com/new"));
        assert!(debug.contains("slug: Some(\"launch\")"), "{debug}");
        assert!(debug.contains("title: None"), "{debug}");
        // A target cannot be cleared.
        assert!(link_body(
            &json!({ "site": 1, "link": "id", "target_url": null }),
            base,
            true
        )
        .is_err());
    }

    #[test]
    fn a_delete_without_confirm_true_is_refused() {
        let e = require_confirm(&json!({ "site": 1, "link": "x" }), "a short link").unwrap_err();
        assert_eq!(e.field(), Some("confirm"));
    }
}

use std::collections::HashMap;

use base64;
use failure::format_err;
use log::{debug, info};
use regex::Regex;
use serde_json;
use serde_json::json;
use serde_derive::{Deserialize, Serialize};
use url::percent_encoding::{DEFAULT_ENCODE_SET, utf8_percent_encode};

use crate::config::JiraConfig;
use crate::errors::*;
use crate::http_client::HTTPClient;
use crate::jira::models::*;
use crate::version;

pub trait Session: Send + Sync {
    fn get_issue(&self, key: &str) -> Result<Issue>;
    fn get_transitions(&self, key: &str) -> Result<Vec<Transition>>;

    fn transition_issue(&self, key: &str, transition: &TransitionRequest) -> Result<()>;

    fn comment_issue(&self, key: &str, comment: &str) -> Result<()>;

    fn add_version(&self, proj: &str, version: &str) -> Result<()>;
    fn get_versions(&self, proj: &str) -> Result<Vec<Version>>;
    fn assign_fix_version(&self, key: &str, version: &str) -> Result<()>;
    fn reorder_version(&self, version: &Version, position: JiraVersionPosition) -> Result<()>;

    fn add_pending_version(&self, key: &str, version: &str) -> Result<()>;
    fn remove_pending_versions(&self, key: &str, versions: &Vec<version::Version>) -> Result<()>;
    fn find_pending_versions(&self, proj: &str) -> Result<HashMap<String, Vec<version::Version>>>;
}

#[derive(Debug)]
pub enum JiraVersionPosition {
    First,
    After(Version),
}

pub struct JiraSession {
    pub client: HTTPClient,
    fix_versions_field: String,
    pending_versions_field: Option<String>,
    pending_versions_field_id: Option<String>,
    restrict_comment_visibility_to_role: Option<String>
}

#[derive(Deserialize)]
struct AuthResp {
    pub name: String,
}

fn lookup_field(field: &str, fields: &Vec<Field>) -> Result<String> {
    fields.iter().find(|f| field == f.id || field == f.name).map(|f| f.id.clone()).ok_or(
        format_err!(
            "Error: Invalid JIRA field: {}",
            field
        ),
    )
}

impl JiraSession {
    pub fn new(config: &JiraConfig) -> Result<JiraSession> {
        let jira_base = config.base_url();
        let api_base = format!("{}/rest/api/2", jira_base);

        let auth = base64::encode(format!("{}:{}", config.username, config.password).as_bytes());

        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::ACCEPT, "application/json".parse().unwrap());
        headers.insert(
            reqwest::header::AUTHORIZATION,
            format!("Basic {}", auth).parse().unwrap(),
        );

        let client = HTTPClient::new_with_headers(&api_base, headers)?;

        let auth_resp = client.get::<AuthResp>(&format!("{}/rest/auth/1/session", jira_base)).map_err(
            |e| format_err!("Error authenticating to JIRA: {}", e),
        )?;
        info!("Logged into JIRA as {}", auth_resp.name);

        let fields = client.get::<Vec<Field>>("/field")?;

        let pending_versions_field_id = match config.pending_versions_field {
            Some(ref f) => Some(lookup_field(f, &fields)?),
            None => None,
        };
        let fix_versions_field = lookup_field(&config.fix_versions(), &fields)?;

        debug!("Pending Version field: {:?}", pending_versions_field_id);
        debug!("Fix Versions field: {:?}", fix_versions_field);

        Ok(JiraSession {
            client: client,
            fix_versions_field: fix_versions_field,
            pending_versions_field: config.pending_versions_field.clone(),
            pending_versions_field_id: pending_versions_field_id,
            restrict_comment_visibility_to_role: config.restrict_comment_visibility_to_role.clone(),
        })
    }
}

impl Session for JiraSession {
    fn get_issue(&self, key: &str) -> Result<Issue> {
        self.client.get::<Issue>(&format!("/issue/{}", key)).map_err(|e| {
            format_err!("Error creating getting issue [{}]: {}", key, e)
        })
    }

    fn get_transitions(&self, key: &str) -> Result<Vec<Transition>> {
        #[derive(Deserialize)]
        struct TransitionsResp {
            transitions: Vec<Transition>,
        }
        let resp = self.client
            .get::<TransitionsResp>(&format!("/issue/{}/transitions?expand=transitions.fields", key))
            .map_err(|e| format_err!("Error creating getting transitions for [{}]: {}", key, e))?;
        Ok(resp.transitions)
    }

    fn transition_issue(&self, key: &str, req: &TransitionRequest) -> Result<()> {
        self.client.post_void(&format!("/issue/{}/transitions", key), &req).map_err(|e| {
            format_err!("Error transitioning [{}]: {}", key, e)
        })
    }

    fn comment_issue(&self, key: &str, comment: &str) -> Result<()> {
        #[derive(Serialize)]
        struct VisibilityReq {
            #[serde(rename = "type")]
            type_name: String,
            value: String,
        }

        #[derive(Serialize)]
        struct CommentReq {
            body: String,
            visibility: Option<VisibilityReq>,
        }

        let mut req = CommentReq {
            body: comment.to_string(),
            visibility: None,
        };

        if let Some(r) = &self.restrict_comment_visibility_to_role {
            req.visibility = Some(VisibilityReq {
                type_name: "role".to_string(),
                value: r.clone(),
            });

            let result = self.client.post_void::<CommentReq>(&format!("/issue/{}/comment", key), &req);
            if result.is_ok() {
                return Ok(());
            }

            req.visibility = None;
            // Fall-through to making the request without the visibility restriction
        }

        self.client.post_void::<CommentReq>(&format!("/issue/{}/comment", key), &req).map_err(
            |e| {
                format_err!("Error commenting on [{}]: {}", key, e)
            },
        )
    }

    fn add_version(&self, proj: &str, version: &str) -> Result<()> {
        #[derive(Serialize)]
        struct AddVersionReq {
            name: String,
            project: String,
        }

        let req = AddVersionReq {
            name: version.into(),
            project: proj.into(),
        };
        self.client.post_void("/version", &req).map_err(|e| {
            format_err!("Error adding version {} to project {}: {}", version, proj, e)
        })
    }

    fn get_versions(&self, proj: &str) -> Result<Vec<Version>> {
        self.client.get::<Vec<Version>>(&format!("/project/{}/versions", proj)).map_err(|e| {
            format_err!("Error getting versions for project {}: {}", proj, e)
        })
    }

    fn assign_fix_version(&self, key: &str, version: &str) -> Result<()> {
        let field = self.fix_versions_field.clone();
        let req = json!({
            "update": {
                field: [{"add" : {"name" : version}}]
            }
        });

        self.client.put_void(&format!("/issue/{}", key), &req).map_err(|e| {
            format_err!("Error adding fix-version {} to [{}]: {}", version, key, e)
        })
    }

    fn reorder_version(&self, version: &Version, position: JiraVersionPosition) -> Result<()> {
        let req = match position {
            JiraVersionPosition::First => {
                json!({
                    "position": "First"
                })
            }
            JiraVersionPosition::After(v) => {
                json!({
                    "after": v.uri
                })
            }
        };

        self.client.post_void(&format!("/version/{}/move", version.id), &req).map_err(|e| {
            format_err!("Error reordering version {}: {}", version.name, e)
        })
    }

    fn add_pending_version(&self, key: &str, version: &str) -> Result<()> {
        if let Some(ref field) = self.pending_versions_field_id.clone() {
            let issue = self.client.get::<serde_json::Value>(&format!("/issue/{}", key))?;

            let version_parsed = match version::Version::parse(version) {
                Some(v) => v,
                None => return Err(format_err!("Unable to parse version: {}", version)),
            };

            let mut pending_versions = parse_pending_version_field(&issue["fields"][field]);
            pending_versions.push(version_parsed);

            pending_versions.sort();
            pending_versions.dedup_by(|a, b| a == b);

            let new_value = pending_versions.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(", ");

            let req = json!({
                "update": {
                    field.to_string(): [{ "set": new_value }]
                }
            });

            self.client.put_void(&format!("/issue/{}", key), &req).map_err(|e| {
                format_err!("Error adding pending version {} to [{}]: {}", version, key, e)
            })?;
        }
        Ok(())
    }


    fn remove_pending_versions(&self, key: &str, versions: &Vec<version::Version>) -> Result<()> {
        if let Some(ref field_id) = self.pending_versions_field_id.clone() {
            let issue = self.client.get::<serde_json::Value>(&format!("/issue/{}", key))?;

            let pending_versions = parse_pending_version_field(&issue["fields"][field_id]);
            let new_pending_versions = pending_versions
                .iter()
                .filter(|v| !versions.contains(v))
                .map(|v| v.to_string())
                .collect::<Vec<_>>()
                .join(", ");

            let req = json!({
                "update": {
                    field_id.to_string(): [{ "set": new_pending_versions }]
                }
            });

            self.client.put_void(&format!("/issue/{}", key), &req).map_err(|e| {
                format_err!("Error removing pending versions {:?} from [{}]: {}", versions, key, e)
            })?;
        }
        Ok(())
    }

    fn find_pending_versions(&self, project: &str) -> Result<HashMap<String, Vec<version::Version>>> {
        if let Some(ref field) = self.pending_versions_field.clone() {
            if let Some(ref field_id) = self.pending_versions_field_id {
                let jql = format!("(project = \"{}\") and \"{}\" is not EMPTY", project, field);
                let search =
                    self.client
                        .get::<serde_json::Value>(
                            &format!("/search?maxResults=5000&jql={}", utf8_percent_encode(&jql, DEFAULT_ENCODE_SET)),
                        )
                        .map_err(|e| {
                            format_err!("Error finding pending pending versions for project {}: {}", project, e)
                        })?;

                return Ok(parse_pending_versions(&search, &field_id));
            }
        }

        Ok(HashMap::new())
    }
}

fn parse_pending_version_field(field: &serde_json::Value) -> Vec<version::Version> {
    let re = Regex::new(r"\s*,\s*").unwrap();
    re.split(field.as_str().unwrap_or("").trim())
        .filter_map(|s| version::Version::parse(s))
        .collect::<Vec<_>>()
}

fn parse_pending_versions(search: &serde_json::Value, field_id: &str) -> HashMap<String, Vec<version::Version>> {
    search["issues"]
        .as_array()
        .unwrap_or(&vec![])
        .into_iter()
        .filter_map(|issue| {
            let key = issue["key"].as_str().unwrap_or("").to_string();
            let list = parse_pending_version_field(&issue["fields"][field_id]);
            if key.is_empty() || list.is_empty() {
                None
            } else {
                Some((key, list))
            }
        })
        .collect::<HashMap<_, _>>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use maplit::hashmap;

    #[test]
    fn test_parse_pending_versions() {
        let search = json!({
            "issues": [
                {
                    "key": "KEY-1",
                    "fields": {}
                },
                {
                    "key": "KEY-2",
                    "fields": {
                        "the-field": "  1.2, 3.4,5,7.7.7  "
                    }
                },
                {
                    "key": "KEY-3",
                    "fields": {
                        "the-field": "1.2,  "
                    }
                }
            ]
        });
        let expected =
            hashmap! {
            "KEY-2".to_string() => vec![
                version::Version::parse("1.2").unwrap(),
                version::Version::parse("3.4").unwrap(),
                version::Version::parse("5").unwrap(),
                version::Version::parse("7.7.7").unwrap()
            ],
            "KEY-3".to_string() => vec![
                version::Version::parse("1.2").unwrap(),
            ],
        };

        let versions = parse_pending_versions(&search, "the-field");
        assert_eq!(expected, versions);
    }
}

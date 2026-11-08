//! Skills (docs/en/design/assistants.md): the Agent Skills format (a `SKILL.md` with `name`
//! and `description` in its header, instructions below, and reference
//! files). The app keeps a library; a role names the skills it uses
//! (`skills: [...]` in its header). Agents get them in their
//! configuration; a built-in agent sees each skill's name and description
//! and loads the rest when it needs it (`load_skill`, `read_skill_file`),
//! the way Claude loads skills on demand. Scripts are not run.

use serde_json::{Value, json};

use super::Hub;
use crate::db::ai::SkillRow;
use crate::logic::{AdminOp, Op};
use crate::model::*;

pub const FILES_PER_SKILL: usize = 20;
pub const FILE_MAX: usize = 512 * 1024;

fn skill_error(rule: &str, m: String) -> ApiError {
    match rule {
        "role.name" => rule!("skill.name", "bad_request", m.replace("role", "skill")),
        "role.size" => rule!("skill.size", "bad_request", m.replace("role", "skill")),
        "role.name_mismatch" => rule!("skill.name_mismatch", "bad_request", m),
        "role.description" => rule!(
            "skill.description",
            "bad_request",
            m.replace("the role", "the skill")
        ),
        _ => rule!("skill.header", "bad_request", m),
    }
}

fn check_path(path: &str) -> ApiResult<()> {
    let ok = !path.is_empty()
        && path.len() <= 200
        && path.split('/').all(|seg| {
            !seg.is_empty()
                && seg != "."
                && seg != ".."
                && seg
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
        });
    if ok {
        Ok(())
    } else {
        Err(rule!(
            "skill.path",
            "bad_request",
            "paths are segments of a-z A-Z 0-9 . _ - separated by /, up to 200 characters"
        ))
    }
}

impl Hub {
    pub async fn put_skill(&self, name: &str, markdown: &str) -> ApiResult<Value> {
        let s = crate::role::parse(name, markdown).map_err(|(r, m)| skill_error(r, m))?;
        self.control()
            .call(Op::Admin(AdminOp::SkillPut {
                name: name.to_string(),
                description: s.description,
                markdown: markdown.to_string(),
                now: now_ms(),
            }))
            .await
    }

    pub async fn skills(&self) -> ApiResult<Vec<Value>> {
        let rows = self
            .read(self.control(), |db| {
                Box::pin(async move { db.skills().await })
            })
            .await?;
        Ok(rows
            .iter()
            .map(|r| json!({"name": r.name, "description": r.description, "version": r.version, "updated_at": r.updated_at}))
            .collect())
    }

    async fn skill_row(&self, name: &str) -> ApiResult<SkillRow> {
        if !crate::role::valid_name(name) {
            return Err(rule!("skill.unknown", "not_found", "no such skill"));
        }
        let n = name.to_string();
        self.read(self.control(), move |db| {
            Box::pin(async move { db.skill(&n).await })
        })
        .await?
        .ok_or_else(|| rule!("skill.unknown", "not_found", "no such skill"))
    }

    /// One skill: its file, instructions and reference files.
    pub async fn skill(&self, name: &str) -> ApiResult<Value> {
        let r = self.skill_row(name).await?;
        let n = name.to_string();
        let files = self
            .read(self.control(), move |db| {
                Box::pin(async move { db.skill_files(&n, false).await })
            })
            .await?;
        let p = crate::role::parse(&r.name, &r.markdown).ok();
        Ok(
            json!({"name": r.name, "description": r.description, "version": r.version, "markdown": r.markdown,
            "instructions": p.map(|p| p.instructions), "files": files.into_iter().map(|(p, _)| p).collect::<Vec<_>>(),
            "updated_at": r.updated_at}),
        )
    }

    pub async fn delete_skill(&self, name: &str) -> ApiResult<Value> {
        self.skill_row(name).await?;
        self.control()
            .call(Op::Admin(AdminOp::SkillDelete {
                name: name.to_string(),
            }))
            .await
    }

    pub async fn put_skill_file(&self, skill: &str, path: &str, body: &[u8]) -> ApiResult<Value> {
        self.skill_row(skill).await?;
        check_path(path)?;
        if body.len() > FILE_MAX {
            return Err(rule!(
                "skill.file_size",
                "bad_request",
                "a skill's file is at most 512 KB"
            ));
        }
        let body = String::from_utf8(body.to_vec()).map_err(|_| {
            rule!(
                "skill.file_encoding",
                "bad_request",
                "skill files are UTF-8 text"
            )
        })?;
        self.control()
            .call(Op::Admin(AdminOp::SkillFilePut {
                skill: skill.to_string(),
                path: path.to_string(),
                body,
                now: now_ms(),
            }))
            .await
    }

    pub async fn delete_skill_file(&self, skill: &str, path: &str) -> ApiResult<Value> {
        self.skill_row(skill).await?;
        self.control()
            .call(Op::Admin(AdminOp::SkillFileDelete {
                skill: skill.to_string(),
                path: path.to_string(),
                now: now_ms(),
            }))
            .await
    }

    /// The skills a role names that the library has: name, description,
    /// instructions and file paths (unknown names are left out).
    pub(super) async fn skills_named(&self, names: &[String]) -> ApiResult<Vec<Value>> {
        let names = names.to_vec();
        let found = self
            .read(self.control(), move |db| {
                Box::pin(async move {
                    let mut out = vec![];
                    for n in names {
                        if let Some(r) = db.skill(&n).await? {
                            let files = db.skill_files(&n, false).await?;
                            out.push((r, files));
                        }
                    }
                    Ok(out)
                })
            })
            .await?;
        Ok(found
            .into_iter()
            .map(|(r, files)| {
                let instructions = crate::role::parse(&r.name, &r.markdown)
                    .map(|p| p.instructions)
                    .unwrap_or_default();
                json!({"name": r.name, "description": r.description, "version": r.version,
                    "instructions": instructions, "markdown": r.markdown,
                    "files": files.into_iter().map(|(p, _)| p).collect::<Vec<_>>()})
            })
            .collect())
    }

    /// One of a skill's files for an agent, when its role names the skill
    /// (`GET /api/agent/skills/{name}/files/{path}`, docs/en/design/assistants.md).
    pub async fn agent_skill_file(
        &self,
        agent: &User,
        skill: &str,
        path: &str,
    ) -> ApiResult<String> {
        let config = self.agent_config(agent).await?;
        let named = config["skills"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|s| s["name"] == skill);
        if !named {
            return Err(rule!("skill.unknown", "not_found", "no such skill"));
        }
        self.skill_file_text(skill, path).await?.ok_or_else(|| {
            rule!(
                "skill.file_unknown",
                "not_found",
                "no such file in this skill"
            )
        })
    }

    /// A skill's reference file, for an agent whose role names the skill.
    pub(super) async fn skill_file_text(
        &self,
        skill: &str,
        path: &str,
    ) -> ApiResult<Option<String>> {
        let (s, p) = (skill.to_string(), path.to_string());
        let files = self
            .read(self.control(), move |db| {
                Box::pin(async move { db.skill_files(&s, true).await })
            })
            .await?;
        Ok(files.into_iter().find(|(x, _)| *x == p).map(|(_, b)| b))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        for ok in ["reference.md", "docs/api.md", "a-b_c.1/x.txt"] {
            assert!(check_path(ok).is_ok(), "{ok}");
        }
        for bad in [
            "", "/a", "a/", "a//b", "../x", "a/../b", "./a", "a b.md", "a\\b",
        ] {
            assert!(check_path(bad).is_err(), "{bad}");
        }
    }
}

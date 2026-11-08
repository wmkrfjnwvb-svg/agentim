//! The built-in library of roles and Skills (docs/en/design/assistants.md,
//! AST-15): what ships with the program, whether the app's library has each
//! item and whether it is the shipped version, and installing items into the
//! app's library. Installing a role also adds the Skills it names that the
//! library lacks; Skills the app already has are left as they are.

use serde_json::{Value, json};

use super::Hub;
use crate::library::{BuiltinSkill, ROLES, SKILLS};
use crate::logic::{AdminOp, LibraryItem, Op};
use crate::model::*;

fn skill_item(s: &BuiltinSkill, explicit: bool) -> ApiResult<LibraryItem> {
    let p =
        crate::role::parse(s.name, s.markdown).map_err(|(_, m)| ApiError::new("internal", m))?;
    Ok(LibraryItem {
        name: s.name.to_string(),
        description: p.description,
        markdown: s.markdown.to_string(),
        files: s
            .files
            .iter()
            .map(|(p, b)| (p.to_string(), b.to_string()))
            .collect(),
        explicit,
    })
}

impl Hub {
    /// The built-in roles: each with its Skills, whether the app's library has
    /// a role of that name (`installed`) and whether it is the shipped text
    /// (`current`), and whether a built-in assistant runs it.
    pub async fn library_roles(&self) -> ApiResult<Vec<Value>> {
        let (have, running) = self
            .read(self.control(), |db| {
                Box::pin(async move {
                    let mut have = vec![];
                    for (name, _) in ROLES {
                        have.push(db.role_get(name).await?.map(|r| r.markdown));
                    }
                    Ok((have, db.assistants().await?))
                })
            })
            .await?;
        let mut out = vec![];
        for ((name, md), lib) in ROLES.iter().zip(have) {
            let r = crate::role::parse(name, md).map_err(|(_, m)| ApiError::new("internal", m))?;
            let assistant = running.iter().any(|a| {
                self.inner
                    .sh
                    .user(&a.agent_id)
                    .is_some_and(|u| u.name == *name)
            });
            out.push(json!({
                "name": name,
                "description": r.description,
                "tags": r.tags,
                "skills": r.skills,
                "instructions": r.instructions,
                "installed": lib.is_some(),
                "current": lib.as_deref() == Some(*md),
                "assistant": assistant,
            }));
        }
        Ok(out)
    }

    /// The built-in Skills: their files, the built-in roles that use them,
    /// and whether the app's library has each (`installed`, `current`).
    pub async fn library_skills(&self) -> ApiResult<Vec<Value>> {
        let have = self
            .read(self.control(), |db| {
                Box::pin(async move {
                    let mut have = vec![];
                    for s in SKILLS {
                        have.push(db.skill(s.name).await?.map(|r| r.markdown));
                    }
                    Ok(have)
                })
            })
            .await?;
        let mut out = vec![];
        for (s, lib) in SKILLS.iter().zip(have) {
            let p = crate::role::parse(s.name, s.markdown)
                .map_err(|(_, m)| ApiError::new("internal", m))?;
            out.push(json!({
                "name": s.name,
                "description": p.description,
                "files": s.files.iter().map(|(p, _)| *p).collect::<Vec<_>>(),
                "used_by": crate::library::roles_using(s.name),
                "installed": lib.is_some(),
                "current": lib.as_deref() == Some(s.markdown),
            }));
        }
        Ok(out)
    }

    /// Copies a built-in role into the app's library, with the Skills it
    /// names that the library lacks. `replace` updates a role already there.
    pub async fn install_library_role(&self, name: &str, replace: bool) -> ApiResult<Value> {
        let md = crate::library::role(name)
            .ok_or_else(|| rule!("role.catalog", "not_found", "no such built-in role"))?;
        let r = crate::role::parse(name, md).map_err(|(_, m)| ApiError::new("internal", m))?;
        let mut skills = vec![];
        for s in &r.skills {
            let b = crate::library::skill(s).ok_or_else(|| {
                ApiError::new("internal", format!("built-in role {name} names {s}"))
            })?;
            skills.push(skill_item(b, false)?);
        }
        self.control()
            .call(Op::Admin(AdminOp::LibraryInstall {
                role: Some(LibraryItem {
                    name: name.to_string(),
                    description: r.description,
                    markdown: md.to_string(),
                    files: vec![],
                    explicit: true,
                }),
                skills,
                replace,
                now: now_ms(),
            }))
            .await
    }

    /// Copies a built-in Skill (with its reference files) into the app's
    /// library. `replace` updates one already there.
    pub async fn install_library_skill(&self, name: &str, replace: bool) -> ApiResult<Value> {
        let s = crate::library::skill(name)
            .ok_or_else(|| rule!("skill.catalog", "not_found", "no such built-in skill"))?;
        self.control()
            .call(Op::Admin(AdminOp::LibraryInstall {
                role: None,
                skills: vec![skill_item(s, true)?],
                replace,
                now: now_ms(),
            }))
            .await
    }
}

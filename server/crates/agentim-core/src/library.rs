//! The built-in library (docs/en/design/assistants.md, AST-15): roles and
//! Skills that ship with the program. They are templates: the console and
//! the admin API list them, and installing one copies it into the app's own
//! library, where the app can change it. A role brings the Skills it names.
//! Nothing here runs by itself; scripts are never part of a Skill.

/// A Skill that ships with the program: its `SKILL.md` and reference files.
pub struct BuiltinSkill {
    pub name: &'static str,
    pub markdown: &'static str,
    pub files: &'static [(&'static str, &'static str)],
}

/// The built-in roles: (name, Markdown), in the order the console shows them.
pub const ROLES: &[(&str, &str)] = &[
    ("assistant", include_str!("../builtin/agents/assistant.md")),
    ("support", include_str!("../builtin/agents/support.md")),
    (
        "order-support",
        include_str!("../builtin/agents/order-support.md"),
    ),
    ("booking", include_str!("../builtin/agents/booking.md")),
    (
        "advisor-aide",
        include_str!("../builtin/agents/advisor-aide.md"),
    ),
    (
        "it-helpdesk",
        include_str!("../builtin/agents/it-helpdesk.md"),
    ),
    (
        "hr-helpdesk",
        include_str!("../builtin/agents/hr-helpdesk.md"),
    ),
    ("sales", include_str!("../builtin/agents/sales.md")),
    ("notes", include_str!("../builtin/agents/notes.md")),
    (
        "translator",
        include_str!("../builtin/agents/translator.md"),
    ),
    ("moderator", include_str!("../builtin/agents/moderator.md")),
    ("task-lead", include_str!("../builtin/agents/task-lead.md")),
];

/// The built-in Skills, by name.
pub const SKILLS: &[BuiltinSkill] = &[
    BuiltinSkill {
        name: "actions-with-approval",
        markdown: include_str!("../builtin/skills/actions-with-approval/SKILL.md"),
        files: &[(
            "references/examples.md",
            include_str!("../builtin/skills/actions-with-approval/references/examples.md"),
        )],
    },
    BuiltinSkill {
        name: "booking-appointments",
        markdown: include_str!("../builtin/skills/booking-appointments/SKILL.md"),
        files: &[],
    },
    BuiltinSkill {
        name: "collect-details-with-forms",
        markdown: include_str!("../builtin/skills/collect-details-with-forms/SKILL.md"),
        files: &[(
            "references/fields.md",
            include_str!("../builtin/skills/collect-details-with-forms/references/fields.md"),
        )],
    },
    BuiltinSkill {
        name: "content-moderation",
        markdown: include_str!("../builtin/skills/content-moderation/SKILL.md"),
        files: &[],
    },
    BuiltinSkill {
        name: "draft-for-review",
        markdown: include_str!("../builtin/skills/draft-for-review/SKILL.md"),
        files: &[],
    },
    BuiltinSkill {
        name: "handoff-to-a-person",
        markdown: include_str!("../builtin/skills/handoff-to-a-person/SKILL.md"),
        files: &[],
    },
    BuiltinSkill {
        name: "knowledge-answers",
        markdown: include_str!("../builtin/skills/knowledge-answers/SKILL.md"),
        files: &[],
    },
    BuiltinSkill {
        name: "meeting-notes",
        markdown: include_str!("../builtin/skills/meeting-notes/SKILL.md"),
        files: &[],
    },
    BuiltinSkill {
        name: "show-data-as-cards",
        markdown: include_str!("../builtin/skills/show-data-as-cards/SKILL.md"),
        files: &[(
            "references/blocks.md",
            include_str!("../builtin/skills/show-data-as-cards/references/blocks.md"),
        )],
    },
    BuiltinSkill {
        name: "summarizing-conversations",
        markdown: include_str!("../builtin/skills/summarizing-conversations/SKILL.md"),
        files: &[],
    },
    BuiltinSkill {
        name: "support-replies",
        markdown: include_str!("../builtin/skills/support-replies/SKILL.md"),
        files: &[],
    },
    BuiltinSkill {
        name: "task-coordination",
        markdown: include_str!("../builtin/skills/task-coordination/SKILL.md"),
        files: &[],
    },
    BuiltinSkill {
        name: "translation",
        markdown: include_str!("../builtin/skills/translation/SKILL.md"),
        files: &[],
    },
    BuiltinSkill {
        name: "troubleshooting",
        markdown: include_str!("../builtin/skills/troubleshooting/SKILL.md"),
        files: &[],
    },
];

/// A built-in role's Markdown.
pub fn role(name: &str) -> Option<&'static str> {
    ROLES.iter().find(|(n, _)| *n == name).map(|(_, md)| *md)
}

/// A built-in Skill.
pub fn skill(name: &str) -> Option<&'static BuiltinSkill> {
    SKILLS.iter().find(|s| s.name == name)
}

/// The built-in roles that name a Skill.
pub fn roles_using(skill: &str) -> Vec<&'static str> {
    ROLES
        .iter()
        .filter(|(n, md)| {
            crate::role::parse(n, md).is_ok_and(|r| r.skills.iter().any(|s| s == skill))
        })
        .map(|(n, _)| *n)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_role_is_valid_and_its_skills_are_built_in() {
        for (name, md) in ROLES {
            let r = crate::role::parse(name, md).unwrap_or_else(|e| panic!("{name}: {e:?}"));
            assert!(
                !r.description.is_empty() && !r.instructions.is_empty(),
                "{name}"
            );
            for s in &r.skills {
                assert!(
                    skill(s).is_some(),
                    "{name} names the skill {s}, which is not built in"
                );
            }
        }
        let mut names: Vec<_> = ROLES.iter().map(|(n, _)| *n).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), ROLES.len(), "role names are unique");
    }

    #[test]
    fn every_skill_is_valid_and_used() {
        for s in SKILLS {
            let p = crate::role::parse(s.name, s.markdown)
                .unwrap_or_else(|e| panic!("{}: {e:?}", s.name));
            assert!(
                !p.description.is_empty() && !p.instructions.is_empty(),
                "{}",
                s.name
            );
            assert!(
                s.files.len() < crate::hub::skills::FILES_PER_SKILL,
                "{}",
                s.name
            );
            for (path, body) in s.files {
                assert!(
                    path.starts_with("references/") && body.len() < crate::hub::skills::FILE_MAX
                );
            }
            assert!(
                !roles_using(s.name).is_empty(),
                "no built-in role uses {}",
                s.name
            );
        }
    }
}

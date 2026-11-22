//! Space-wide Composable Skills Library (`Team → Skills`): master-detail
//! browser, starter playbook templates, Markdown write/preview editor, version
//! history & rollback, and agent usage chips.

use gpui_kit::component::button::{ButtonGroup, ButtonVariants as _};
use gpui_kit::component::input::Textarea;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _, h_flex, v_flex,
};
use gpui_kit::*;
use threadlane_desktop::{SKILL_STARTERS, When, count_form};

use super::{gap_line, menu_item};
use crate::components::{button, card, count, empty_state, field, meta, section};
use crate::icons::AppIcon;
use crate::tokens::{
    BADGE_RADIUS, FIELD_RADIUS, LG, MD, MEMORY_LIST_WIDTH, ROW_RADIUS, SCROLLBAR_GUTTER, SM,
    Status, TYPE_BODY, TYPE_META, XS,
};
use crate::{Desktop, Screen};

impl Desktop {
    pub(super) fn skills_workspace(
        &self,
        scope: Option<&str>,
        when: &When,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let active_space = scope
            .map(str::to_owned)
            .or_else(|| self.state.skills_space().map(str::to_owned));
        let Some(space) = active_space else {
            return empty_state("No space paired yet.", None, cx).into_any_element();
        };

        let multi_space = scope.is_none() && self.state.spaces().len() > 1;
        let space_bar = multi_space.then(|| {
            h_flex()
                .gap(XS)
                .flex_wrap()
                .items_center()
                .child(meta("Space", cx))
                .children(self.state.spaces().iter().map(|known| {
                    let name = known.name.clone();
                    let active = name == space;
                    button(SharedString::from(format!("skills-space-{name}")))
                        .small()
                        .outline()
                        .label(name.clone())
                        .selected(active)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.state.set_skills_space(&name);
                            this.sync_selected_skill_editor(&name);
                            this.load_space_skills(name.clone(), cx);
                            cx.notify();
                        }))
                }))
        });

        let skills = self.state.space_skills(&space);
        let creating = self.state.skill_creating(&space);
        let selected_slug = self.state.skill_selected_slug(&space).map(str::to_owned);
        let new_space = space.clone();

        let mut list = v_flex()
            .w(MEMORY_LIST_WIDTH)
            .flex_none()
            .h_full()
            .overflow_y_scrollbar()
            .id("skills-list")
            .gap(SM)
            .pr(SCROLLBAR_GUTTER)
            .child(
                h_flex()
                    .justify_between()
                    .items_center()
                    .gap(SM)
                    .child(section("Playbooks").children(count(skills.len(), cx)))
                    .child(
                        button("new-skill")
                            .small()
                            .outline()
                            .icon(AppIcon::Plus)
                            .label("New skill")
                            .disabled(creating)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.start_new_space_skill(new_space.clone(), cx);
                            })),
                    ),
            );

        if let Some(gap) = self.state.skills_gap(&space) {
            list = list.child(gap_line(&gap, "skills", &space, cx));
        } else if skills.is_empty() && !creating {
            list = list.child(empty_state(
                "No space skills yet.",
                Some("Create one or start from a template below."),
                cx,
            ));
            for starter in SKILL_STARTERS {
                let starter_space = space.clone();
                list = list.child(
                    card(cx)
                        .p(SM)
                        .child(
                            h_flex()
                                .justify_between()
                                .items_center()
                                .gap(SM)
                                .child(
                                    v_flex()
                                        .flex_1()
                                        .min_w_0()
                                        .gap(XS)
                                        .child(
                                            div()
                                                .text_size(TYPE_BODY)
                                                .font_weight(FontWeight::MEDIUM)
                                                .child(starter.title),
                                        )
                                        .child(
                                            meta(format!("/skill:{}", starter.slug), cx)
                                                .font_family(cx.theme().mono_font_family.clone()),
                                        ),
                                )
                                .child(
                                    button(SharedString::from(format!(
                                        "seed-skill-{}",
                                        starter.slug
                                    )))
                                    .small()
                                    .outline()
                                    .icon(AppIcon::Plus)
                                    .label("Add")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.save_starter_skill(
                                            starter_space.clone(),
                                            starter.slug.to_owned(),
                                            starter.content.to_owned(),
                                            cx,
                                        );
                                    })),
                                ),
                        )
                        .child(meta(starter.summary, cx).whitespace_normal()),
                );
            }
        }

        for skill in skills {
            let slug = skill.slug.clone();
            let select_space = space.clone();
            let selected = !creating && selected_slug.as_deref() == Some(slug.as_str());
            let used_count = self.state.skill_used_by(&space, &slug).len();
            let trailing = (used_count > 0).then(|| SharedString::from(used_count.to_string()));
            let row_btn = crate::components::row(
                format!("skill-row-{space}-{slug}"),
                AppIcon::BookOpen,
                skill.title.clone(),
                trailing,
                false,
                selected,
                cx,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.select_space_skill(select_space.clone(), slug.clone(), cx);
            }));
            let view = cx.entity().downgrade();
            let copy_mention = format!("/skill:{}", skill.slug);
            let ctx_row = component::menu::ContextMenuExt::context_menu(
                v_flex().child(row_btn).child(
                    meta(format!("/skill:{}", skill.slug), cx)
                        .font_family(cx.theme().mono_font_family.clone())
                        .px(MD),
                ),
                move |menu, _, _| {
                    let mention = copy_mention.clone();
                    menu.item(menu_item(
                        &view,
                        "Copy /skill mention",
                        move |this, _, cx| {
                            this.copy(mention.clone(), cx);
                        },
                    ))
                },
            );
            list = list.child(ctx_row);
        }

        let detail = self.skill_detail_pane(&space, when, cx);

        v_flex()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .gap(MD)
            .children(space_bar)
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_start()
                    .gap(LG)
                    .child(list)
                    .child(detail),
            )
            .into_any_element()
    }

    fn skill_detail_pane(
        &self,
        space: &str,
        when: &When,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut detail = v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scrollbar()
            .id("skill-detail")
            .gap(SM)
            .pr(SCROLLBAR_GUTTER);
        let saving = self.state.skill_saving(space);
        let preview = self.state.skill_preview(space);
        let error = self.state.skill_error(space).map(str::to_owned);
        let mode_space = space.to_owned();
        let mode_toggle = ButtonGroup::new("skill-editor-mode")
            .outline()
            .compact()
            .child(button("skill-write").label("Write").selected(!preview))
            .child(button("skill-preview").label("Preview").selected(preview))
            .on_click(cx.listener(move |this, clicked: &Vec<usize>, _, cx| {
                this.state
                    .set_skill_preview(&mode_space, clicked.first() == Some(&1));
                cx.notify();
            }));

        if self.state.skill_creating(space) {
            let save_space = space.to_owned();
            let cancel_space = space.to_owned();
            let actions = h_flex()
                .gap(SM)
                .flex_wrap()
                .child(mode_toggle)
                .child(
                    button("skill-create-save")
                        .small()
                        .primary()
                        .icon(AppIcon::Save)
                        .label(if saving { "Saving…" } else { "Save skill" })
                        .disabled(saving)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.save_space_skill(save_space.clone(), cx);
                        })),
                )
                .child(
                    button("skill-create-cancel")
                        .small()
                        .ghost()
                        .icon(AppIcon::X)
                        .label("Cancel")
                        .disabled(saving)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.state.cancel_new_skill(&cancel_space);
                            this.sync_selected_skill_editor(&cancel_space);
                            cx.notify();
                        })),
                );
            detail = detail
                .child(
                    h_flex()
                        .justify_between()
                        .items_center()
                        .flex_wrap()
                        .gap(SM)
                        .child(section("New skill"))
                        .child(actions),
                )
                .child(
                    v_flex()
                        .gap(XS)
                        .child(meta("Slug", cx))
                        .child(field(&self.skill_slug).aria_label("Skill slug"))
                        .child(meta(
                            "Lowercase letters, digits, '-' or '_'. Invoke anywhere with /skill:<slug>.",
                            cx,
                        )),
                );
        } else if let Some(skill) = self.state.skill_selected(space).cloned() {
            let save_space = space.to_owned();
            let del_space = space.to_owned();
            let del_slug = skill.slug.clone();
            let mention = format!("/skill:{}", skill.slug);
            let used_by = self.state.skill_used_by(space, &skill.slug);

            let actions = h_flex()
                .gap(SM)
                .flex_wrap()
                .child(mode_toggle)
                .child(
                    button("skill-save")
                        .small()
                        .primary()
                        .icon(AppIcon::Save)
                        .label(if saving { "Saving…" } else { "Save" })
                        .disabled(saving)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.save_space_skill(save_space.clone(), cx);
                        })),
                )
                .child(
                    button("skill-delete")
                        .small()
                        .ghost()
                        .icon(AppIcon::Trash)
                        .label("Delete skill…")
                        .disabled(saving)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.confirm_action(
                                threadlane_desktop::Consequence::DeleteSkill(
                                    del_space.clone(),
                                    del_slug.clone(),
                                ),
                                window,
                                cx,
                            );
                        })),
                );

            let mut used_row = h_flex().gap(XS).flex_wrap().items_center();
            if used_by.is_empty() {
                used_row = used_row.child(meta(
                    "Not attached to any agent yet — toggle on an agent or type its /skill mention in chat.",
                    cx,
                ));
            } else {
                used_row = used_row.child(meta(
                    format!(
                        "Attached to {} {}:",
                        used_by.len(),
                        count_form(used_by.len(), "agent", "agents")
                    ),
                    cx,
                ));
                for agent in used_by {
                    let agent_space = space.to_owned();
                    let agent_name = agent.name.clone();
                    used_row = used_row.child(
                        button(SharedString::from(format!(
                            "skill-used-by-{space}-{}",
                            agent.name
                        )))
                        .small()
                        .outline()
                        .icon(AppIcon::Bot)
                        .label(agent.name.clone())
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.open_agent_edit(&agent_space, &agent_name, window, cx);
                                this.go(Screen::Agents(Some(agent_space.clone())), cx);
                            },
                        )),
                    );
                }
            }

            detail = detail
                .child(
                    h_flex()
                        .justify_between()
                        .items_start()
                        .flex_wrap()
                        .gap(SM)
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap(XS)
                                .child(section(skill.title.clone()))
                                .child(
                                    h_flex()
                                        .gap(SM)
                                        .flex_wrap()
                                        .items_center()
                                        .child(
                                            self.copyable("copy-skill-mention", mention, cx)
                                                .font_family(cx.theme().mono_font_family.clone()),
                                        )
                                        .child(
                                            Tag::secondary()
                                                .small()
                                                .rounded(BADGE_RADIUS)
                                                .text_size(TYPE_META)
                                                .child(format!("v{}", skill.version)),
                                        )
                                        .child(meta(
                                            format!("Updated {}", when.moment(skill.written_at)),
                                            cx,
                                        )),
                                ),
                        )
                        .child(actions),
                )
                .child(used_row);
        } else {
            return detail.child(meta(
                "Select a skill playbook on the left or create a new one to share across agents and tasks.",
                cx,
            ));
        }

        if let Some(why) = error {
            detail = detail.child(
                meta(format!("Could not save skill: {why}"), cx)
                    .text_color(Status::Failed.colors(cx.theme()).0),
            );
        }

        if preview {
            let text = self.skill_editor.read(cx).value().to_string();
            detail = detail.child(
                div()
                    .p(SM)
                    .min_h_32()
                    .text_size(TYPE_BODY)
                    .bg(cx.theme().muted)
                    .rounded(ROW_RADIUS)
                    .whitespace_normal()
                    .child(self.prose(format!("skill-preview-{space}"), space, None, text, cx)),
            );
        } else {
            detail = detail.child(
                Textarea::new(&self.skill_editor)
                    .rounded(FIELD_RADIUS)
                    .aria_label("Skill playbook Markdown"),
            );
        }

        if let Some(skill) = self.state.skill_selected(space).cloned() {
            detail = detail.child(section("Versions"));
            let versions = self.state.skill_version_rows(space, when);
            if let Some(gap) = self.state.skill_versions_gap(space) {
                detail = detail.child(gap_line(&gap, "skill versions", space, cx));
            } else if versions.is_empty() {
                detail = detail.child(meta("No saved versions yet.", cx));
            }
            for row in versions {
                let inspect_space = space.to_owned();
                let label = if row.current {
                    format!("{} · current", row.label)
                } else {
                    row.label
                };
                let v = row.version;
                detail = detail.child(
                    button(SharedString::from(format!(
                        "skill-version-{space}-{}-{v}",
                        skill.slug
                    )))
                    .ghost()
                    .label(label.clone())
                    .accessibility_label(format!("Inspect {label}"))
                    .tab_stop(true)
                    .selected(row.selected)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.state.inspect_skill_version(&inspect_space, Some(v));
                        cx.notify();
                    })),
                );
            }
            if let Some(version) = self.state.skill_inspected(space).cloned() {
                let restore_space = space.to_owned();
                let restore_slug = skill.slug.clone();
                let ver_num = version.version;
                let can_restore = ver_num != skill.version && !version.deleted;
                detail = detail.child(
                    h_flex()
                        .justify_between()
                        .items_center()
                        .gap(SM)
                        .child(section(format!("Version {ver_num} · read-only")))
                        .children(can_restore.then(|| {
                            button("restore-skill-version")
                                .small()
                                .outline()
                                .icon(AppIcon::Copy)
                                .label("Restore this version")
                                .disabled(saving)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.restore_space_skill_version(
                                        restore_space.clone(),
                                        restore_slug.clone(),
                                        ver_num,
                                        cx,
                                    );
                                }))
                        })),
                );
                if version.deleted {
                    detail = detail.child(meta(
                        "This version records a deletion; it has no file contents.",
                        cx,
                    ));
                } else {
                    detail = detail.child(
                        div()
                            .p(SM)
                            .text_size(TYPE_BODY)
                            .bg(cx.theme().muted)
                            .rounded(ROW_RADIUS)
                            .whitespace_normal()
                            .child(self.prose(
                                format!("skill-history-{space}-{}-{ver_num}", skill.slug),
                                space,
                                None,
                                version.content,
                                cx,
                            )),
                    );
                }
            }
        }

        detail
    }
}

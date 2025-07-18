use std::fmt::Write;

use bathbot_macros::PaginationBuilder;
use bathbot_util::{
    CowUtils, EmbedBuilder, FooterBuilder, ModsFormatter, ScoreExt,
    constants::OSU_BASE,
    datetime::HowLongAgoDynamic,
    numbers::{WithComma, round},
};
use eyre::Result;
use rosu_v2::prelude::GameMode;
use twilight_model::{
    channel::message::Component,
    id::{Id, marker::UserMarker},
};

use crate::{
    active::{
        BuildPage, ComponentResult, IActiveMessage,
        pagination::{Pages, handle_pagination_component, handle_pagination_modal},
    },
    commands::osu::TopIfEntry,
    embeds::{ComboFormatter, HitResultFormatter, PpFormatter},
    manager::redis::osu::CachedUser,
    util::{
        CachedUserExt,
        interaction::{InteractionComponent, InteractionModal},
        osu::GradeFormatter,
    },
};

#[derive(PaginationBuilder)]
pub struct TopIfPagination {
    user: CachedUser,
    #[pagination(per_page = 5)]
    entries: Box<[TopIfEntry]>,
    mode: GameMode,
    pre_pp: f32,
    post_pp: f32,
    rank: Option<u32>,
    content: Box<str>,
    msg_owner: Id<UserMarker>,
    pages: Pages,
}

impl IActiveMessage for TopIfPagination {
    async fn build_page(&mut self) -> Result<BuildPage> {
        let pages = &self.pages;
        let end_idx = self.entries.len().min(pages.index() + pages.per_page());
        let entries = &self.entries[pages.index()..end_idx];

        let mut description = String::with_capacity(512);

        for entry in entries {
            let TopIfEntry {
                original_idx,
                score,
                old_pp,
                map,
                stars,
                max_pp,
                max_combo,
            } = entry;

            let _ = writeln!(
                description,
                "**#{original_idx} [{title} [{version}]]({OSU_BASE}b/{id}) +{mods}** [{stars:.2}★]\n\
                {grade} {old_pp:.2} → {pp} • {acc}% • {score}\n\
                [ {combo} ] • {hits} • {ago}",
                title = map.title().cow_escape_markdown(),
                version = map.version().cow_escape_markdown(),
                id = map.map_id(),
                mods = ModsFormatter::new(&score.mods, score.is_legacy),
                grade =
                    GradeFormatter::new(score.grade, Some(score.score_id), entry.score.is_legacy()),
                pp = PpFormatter::new(Some(score.pp), Some(*max_pp)),
                acc = round(score.accuracy),
                score = WithComma::new(score.score),
                combo = ComboFormatter::new(score.max_combo, Some(*max_combo)),
                hits = HitResultFormatter::new(self.mode, &score.statistics),
                ago = HowLongAgoDynamic::new(&score.ended_at)
            );
        }

        description.pop();

        let mut footer_text = format!("Page {}/{}", pages.curr_page(), pages.last_page());

        if let Some(rank) = self.rank {
            let _ = write!(
                footer_text,
                " • The current rank for {pp}pp is approx. #{rank}",
                pp = WithComma::new(self.post_pp),
                rank = WithComma::new(rank)
            );
        }

        let title = format!(
            "Total pp: {pre_pp} → **{post_pp}pp** ({pp_diff:+})",
            pre_pp = self.pre_pp,
            post_pp = self.post_pp,
            pp_diff = (100.0 * (self.post_pp - self.pre_pp)).round() / 100.0,
        );

        let embed = EmbedBuilder::new()
            .author(self.user.author_builder(false))
            .description(description)
            .footer(FooterBuilder::new(footer_text))
            .thumbnail(self.user.avatar_url.as_ref())
            .title(title);

        Ok(BuildPage::new(embed, false).content(self.content.clone()))
    }

    fn build_components(&self) -> Vec<Component> {
        self.pages.components()
    }

    async fn handle_component(&mut self, component: &mut InteractionComponent) -> ComponentResult {
        handle_pagination_component(component, self.msg_owner, false, &mut self.pages).await
    }

    async fn handle_modal(&mut self, modal: &mut InteractionModal) -> Result<()> {
        handle_pagination_modal(modal, self.msg_owner, false, &mut self.pages).await
    }
}

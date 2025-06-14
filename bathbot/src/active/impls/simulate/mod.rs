use std::{
    borrow::Cow,
    fmt::{Debug, Formatter, Result as FmtResult, Write},
    mem,
    str::FromStr,
};

use bathbot_util::{
    Authored, CowUtils, EmbedBuilder, FooterBuilder,
    constants::OSU_BASE,
    datetime::SecToMinSec,
    fields,
    modal::{ModalBuilder, TextInputBuilder},
    numbers::{WithComma, round},
    osu::calculate_grade,
};
use eyre::{ContextCompat, Report, Result};
use rosu_pp::{
    Beatmap,
    model::{
        hit_object::{HitObjectKind, HoldNote, Spinner},
        mode::GameMode as Mode,
    },
};
use rosu_v2::{
    model::mods::{
        GameMod, GameMods,
        generated_mods::{
            DifficultyAdjustCatch, DifficultyAdjustMania, DifficultyAdjustOsu,
            DifficultyAdjustTaiko,
        },
    },
    mods,
    prelude::{GameMode, GameModsIntermode, Grade},
};
use twilight_model::{
    channel::message::{Component, embed::EmbedField},
    id::{Id, marker::UserMarker},
};

pub use self::{attrs::SimulateAttributes, data::SimulateData, top_old::TopOldVersion};
use crate::{
    active::{
        BuildPage, ComponentResult, IActiveMessage,
        impls::simulate::data::{ComboOrRatio, SimulateValues, StateOrScore},
    },
    commands::osu::parsed_map::AttachedSimulateMap,
    embeds::{ComboFormatter, HitResultFormatter, KeyFormatter, PpFormatter},
    manager::OsuMap,
    util::{
        ComponentExt, Emote, ModalExt,
        interaction::{InteractionComponent, InteractionModal},
        osu::{GradeCompletionFormatter, MapInfo},
    },
};

mod attrs;
mod data;
mod state;
mod top_old;

pub struct SimulateComponents {
    map: SimulateMap,
    data: SimulateData,
    defer: bool,
    msg_owner: Id<UserMarker>,
}

impl IActiveMessage for SimulateComponents {
    async fn build_page(&mut self) -> Result<BuildPage> {
        {
            let pp_map = self.map.pp_map_mut();

            if let Some(ar) = self.data.attrs.ar {
                pp_map.ar = ar;
            }

            if let Some(cs) = self.data.attrs.cs {
                pp_map.cs = cs;
            }

            if let Some(hp) = self.data.attrs.hp {
                pp_map.hp = hp;
            }

            if let Some(od) = self.data.attrs.od {
                pp_map.od = od;
            }
        }

        let mut title = match self.map {
            SimulateMap::Full(ref map) => {
                format!(
                    "{} - {} [{}]",
                    map.artist().cow_escape_markdown(),
                    map.title().cow_escape_markdown(),
                    map.version().cow_escape_markdown(),
                )
            }
            SimulateMap::Attached(ref map) => map.filename.as_ref().to_owned(),
        };

        if matches!(self.data.version, TopOldVersion::Mania(_)) {
            let _ = write!(
                title,
                " {}",
                KeyFormatter::new(&mods!(Mania), self.map.pp_map().cs)
            );
        }

        let footer_text = match self.map {
            SimulateMap::Full(ref map) => {
                format!(
                    "{:?} mapset of {} • {version}",
                    map.status(),
                    map.creator(),
                    version = self.data.version
                )
            }
            SimulateMap::Attached(_) => self.data.version.to_string(),
        };

        let mut footer = FooterBuilder::new(footer_text);

        if let SimulateMap::Full(ref map) = self.map {
            footer = footer.icon_url(Emote::from(map.mode()).url());
        }

        let image = match self.map {
            SimulateMap::Full(ref map) => Some(map.cover().to_owned()),
            SimulateMap::Attached(_) => None,
        };

        let url = match self.map {
            SimulateMap::Full(ref map) => Some(format!("{OSU_BASE}b/{}", map.map_id())),
            SimulateMap::Attached(_) => None,
        };

        let SimulateValues {
            stars,
            pp,
            max_pp,
            clock_rate,
            combo_ratio,
            score_state,
        } = self.data.simulate(&self.map);

        let mods = self
            .data
            .mods
            .as_ref()
            .map(Cow::Borrowed)
            .unwrap_or_default();

        let mut grade = if mods.contains_any(mods!(HD FL)) {
            Grade::XH
        } else {
            Grade::X
        };

        let mut too_suspicious = false;

        let (score, acc, hits) = match score_state {
            StateOrScore::Score(score) => {
                let score = EmbedField {
                    inline: true,
                    name: "Score".to_owned(),
                    value: WithComma::new(score).to_string(),
                };

                (Some(score), None, None)
            }
            StateOrScore::State(state) => {
                let map = self.data.set_on_lazer.then_some(self.map.pp_map());

                let (mode, stats, max_stats) = state.into_parts(map);
                let mods = mods.as_ref();

                let max_stats_opt = self.data.set_on_lazer.then_some(&max_stats);
                grade = calculate_grade(mode, mods, &stats, max_stats_opt);

                let acc = if self.data.set_on_lazer {
                    stats.accuracy(mode, &max_stats)
                } else {
                    stats.legacy_accuracy(mode)
                };

                let acc = EmbedField {
                    inline: true,
                    name: "Acc".to_owned(),
                    value: format!("{}%", round(acc)),
                };

                let hits = EmbedField {
                    inline: true,
                    name: "Hits".to_owned(),
                    value: HitResultFormatter::new(mode, &stats).to_string(),
                };

                (None, Some(acc), Some(hits))
            }
            StateOrScore::Neither => {
                too_suspicious = true;

                (None, None, None)
            }
        };

        let (combo, ratio) = match combo_ratio {
            ComboOrRatio::Combo { score, max } => {
                let combo = EmbedField {
                    inline: true,
                    name: "Combo".to_owned(),
                    value: ComboFormatter::new(score, Some(max)).to_string(),
                };

                (Some(combo), None)
            }
            ComboOrRatio::Ratio(ratio) => {
                let ratio = EmbedField {
                    inline: true,
                    name: "Ratio".to_owned(),
                    value: ratio.to_string(),
                };

                (None, Some(ratio))
            }
            ComboOrRatio::Neither => (None, None),
        };

        let n_objects = self.map.n_objects();
        let mut fields = Vec::new();

        if too_suspicious {
            fields![fields {
                "Map too suspicious",
                "Skipped calculating attributes".to_owned(),
                false;
            }];
        } else {
            let grade = GradeCompletionFormatter::new_without_score(
                &mods,
                grade,
                n_objects,
                self.map.mode(),
                n_objects,
                // Could use `self.data.set_on_lazer` but using the legacy
                // formatting of mods does not show custom rates & co so it's
                // probably best to always use the new formatting
                false,
            );

            fields![fields { "Grade", grade.to_string(), true; }];
        }

        if let Some(acc) = acc {
            fields.push(acc);
        }

        if let Some(score) = score {
            fields.push(score);
        }

        if let Some(ratio) = ratio {
            fields.push(ratio);
        }

        if let Some(combo) = combo {
            fields.push(combo);
        }

        if !too_suspicious {
            fields![fields {
                "PP",
                PpFormatter::new(Some(pp), Some(max_pp)).to_string(),
                true;
            }];
        }

        if let Some(clock_rate) = clock_rate {
            fields![fields { "Clock rate", format!("{clock_rate:.2}"), true }];
        }

        if let Some(hits) = hits {
            fields.push(hits);
        }

        let map_info = self
            .map
            .map_info(stars, mods.as_ref(), self.data.clock_rate);
        fields![fields { "Map Info", map_info, false; }];

        let mut embed = EmbedBuilder::new()
            .fields(fields)
            .footer(footer)
            .title(title);

        if let Some(image) = image {
            embed = embed.image(image);
        }

        if let Some(url) = url {
            embed = embed.url(url);
        }

        let content = "Simulated score:";
        let defer = mem::replace(&mut self.defer, true);

        Ok(BuildPage::new(embed, defer).content(content))
    }

    fn build_components(&self) -> Vec<Component> {
        self.data.version.components(self.data.set_on_lazer)
    }

    async fn handle_component(&mut self, component: &mut InteractionComponent) -> ComponentResult {
        let user_id = match component.user_id() {
            Ok(user_id) => user_id,
            Err(err) => return ComponentResult::Err(err),
        };

        if user_id != self.msg_owner {
            return ComponentResult::Ignore;
        }

        let modal = match component.data.custom_id.as_str() {
            "sim_mods" => {
                let input = TextInputBuilder::new("sim_mods", "Mods")
                    .placeholder("E.g. hd or HdHRdteZ")
                    .required(false);

                ModalBuilder::new("sim_mods", "Specify mods").input(input)
            }
            "sim_combo" => {
                let input = TextInputBuilder::new("sim_combo", "Combo")
                    .placeholder("Integer")
                    .required(false);

                ModalBuilder::new("sim_combo", "Specify combo").input(input)
            }
            "sim_acc" => {
                let input = TextInputBuilder::new("sim_acc", "Accuracy")
                    .placeholder("Number")
                    .required(false);

                ModalBuilder::new("sim_acc", "Specify accuracy").input(input)
            }
            "sim_geki" => {
                let input = TextInputBuilder::new("sim_geki", "Amount of gekis")
                    .placeholder("Integer")
                    .required(false);

                ModalBuilder::new("sim_geki", "Specify the amount of gekis").input(input)
            }
            "sim_katu" => {
                let input = TextInputBuilder::new("sim_katu", "Amount of katus")
                    .placeholder("Integer")
                    .required(false);

                ModalBuilder::new("sim_katu", "Specify the amount of katus").input(input)
            }
            "sim_n300" => {
                let input = TextInputBuilder::new("sim_n300", "Amount of 300s")
                    .placeholder("Integer")
                    .required(false);

                ModalBuilder::new("sim_n300", "Specify the amount of 300s").input(input)
            }
            "sim_n100" => {
                let input = TextInputBuilder::new("sim_n100", "Amount of 100s")
                    .placeholder("Integer")
                    .required(false);

                ModalBuilder::new("sim_n100", "Specify the amount of 100s").input(input)
            }
            "sim_n50" => {
                let input = TextInputBuilder::new("sim_n50", "Amount of 50s")
                    .placeholder("Integer")
                    .required(false);

                ModalBuilder::new("sim_n50", "Specify the amount of 50s").input(input)
            }
            "sim_miss" => {
                let input = TextInputBuilder::new("sim_miss", "Amount of misses")
                    .placeholder("Integer")
                    .required(false);

                ModalBuilder::new("sim_miss", "Specify the amount of misses").input(input)
            }
            "sim_lazer" => {
                self.data.set_on_lazer = true;
                self.defer = false;

                return ComponentResult::BuildPage;
            }
            "sim_stable" => {
                self.data.set_on_lazer = false;
                self.data.n_slider_ends = None;
                self.data.n_large_ticks = None;
                self.defer = false;

                return ComponentResult::BuildPage;
            }
            "sim_slider_ends" => {
                let input = TextInputBuilder::new("sim_slider_ends", "Amount of slider end hits")
                    .placeholder("Integer")
                    .required(false);

                ModalBuilder::new("sim_slider_ends", "Specify the amount of slider end hits")
                    .input(input)
            }
            "sim_large_ticks" => {
                let input = TextInputBuilder::new(
                    "sim_large_ticks",
                    "Amount of large tick hits (ticks & reverses)",
                )
                .placeholder("Integer")
                .required(false);

                ModalBuilder::new("sim_large_ticks", "Specify the amount of large tick hits")
                    .input(input)
            }
            "sim_score" => {
                let input = TextInputBuilder::new("sim_score", "Score")
                    .placeholder("Integer")
                    .required(false);

                ModalBuilder::new("sim_score", "Specify the score").input(input)
            }
            "sim_clock_rate" => {
                let clock_rate = TextInputBuilder::new("sim_clock_rate", "Clock rate")
                    .placeholder("Specify a clock rate")
                    .required(false);

                let bpm = TextInputBuilder::new(
                    "sim_bpm",
                    "BPM (overwritten if clock rate is specified)",
                )
                .placeholder("Specify a BPM")
                .required(false);

                ModalBuilder::new("sim_speed_adjustments", "Speed adjustments")
                    .input(clock_rate)
                    .input(bpm)
            }
            "sim_attrs" => {
                let ar = TextInputBuilder::new("sim_ar", "AR")
                    .placeholder("Specify an approach rate")
                    .required(false);

                let cs = TextInputBuilder::new("sim_cs", "CS")
                    .placeholder("Specify a circle size")
                    .required(false);

                let hp = TextInputBuilder::new("sim_hp", "HP")
                    .placeholder("Specify a drain rate")
                    .required(false);

                let od = TextInputBuilder::new("sim_od", "OD")
                    .placeholder("Specify an overall difficulty")
                    .required(false);

                ModalBuilder::new("sim_attrs", "Attributes")
                    .input(ar)
                    .input(cs)
                    .input(hp)
                    .input(od)
            }
            "sim_osu_version" | "sim_taiko_version" | "sim_catch_version" | "sim_mania_version" => {
                return self.handle_topold_menu(component).await;
            }
            other => {
                warn!(name = %other, ?component, "Unknown simulate component");

                return ComponentResult::Ignore;
            }
        };

        ComponentResult::CreateModal(modal)
    }

    async fn handle_modal(&mut self, modal: &mut InteractionModal) -> Result<()> {
        if modal.user_id()? != self.msg_owner {
            return Ok(());
        }

        let input = modal
            .data
            .components
            .first()
            .and_then(|row| row.components.first())
            .wrap_err("Missing simulate modal input")?
            .value
            .as_deref()
            .filter(|val| !val.is_empty());

        match modal.data.custom_id.as_str() {
            "sim_mods" => {
                let mods_res = input.map(|s| {
                    s.trim_start_matches('+')
                        .trim_end_matches('!')
                        .parse::<GameModsIntermode>()
                });

                let mods = match mods_res {
                    Some(Ok(value)) => Some(value),
                    Some(Err(_)) => {
                        debug!(input, "Failed to parse simulate mods");

                        return Ok(());
                    }
                    None => None,
                };

                match mods.map(|mods| mods.try_with_mode(self.map.mode())) {
                    Some(Some(mods)) if mods.is_valid() => self.data.mods = Some(mods),
                    None => self.data.mods = None,
                    Some(Some(mods)) => {
                        debug!("Incompatible mods {mods}");

                        return Ok(());
                    }
                    Some(None) => {
                        debug!(input, "Invalid mods for mode");

                        return Ok(());
                    }
                }
            }
            "sim_acc" => match input.map(str::parse::<f32>) {
                Some(Ok(value)) => self.data.acc = Some(value.clamp(0.0, 100.0)),
                Some(Err(_)) => {
                    debug!(input, "Failed to parse simulate accuracy");

                    return Ok(());
                }
                None => self.data.acc = None,
            },
            "sim_combo" => match input.map(str::parse) {
                Some(Ok(value)) => self.data.combo = Some(value),
                Some(Err(_)) => {
                    debug!(input, "Failed to parse simulate combo");

                    return Ok(());
                }
                None => self.data.combo = None,
            },
            "sim_geki" => match input.map(str::parse) {
                Some(Ok(value)) => self.data.n_geki = Some(value),
                Some(Err(_)) => {
                    debug!(input, "Failed to parse simulate gekis");

                    return Ok(());
                }
                None => self.data.n_geki = None,
            },
            "sim_katu" => match input.map(str::parse) {
                Some(Ok(value)) => self.data.n_katu = Some(value),
                Some(Err(_)) => {
                    debug!(input, "Failed to parse simulate katus");

                    return Ok(());
                }
                None => self.data.n_katu = None,
            },
            "sim_n300" => match input.map(str::parse) {
                Some(Ok(value)) => self.data.n300 = Some(value),
                Some(Err(_)) => {
                    debug!(input, "Failed to parse simulate 300s");

                    return Ok(());
                }
                None => self.data.n300 = None,
            },
            "sim_n100" => match input.map(str::parse) {
                Some(Ok(value)) => self.data.n100 = Some(value),
                Some(Err(_)) => {
                    debug!(input, "Failed to parse simulate 100s");

                    return Ok(());
                }
                None => self.data.n100 = None,
            },
            "sim_n50" => match input.map(str::parse) {
                Some(Ok(value)) => self.data.n50 = Some(value),
                Some(Err(_)) => {
                    debug!(input, "Failed to parse simulate 50s");

                    return Ok(());
                }
                None => self.data.n50 = None,
            },
            "sim_miss" => match input.map(str::parse) {
                Some(Ok(value)) => self.data.n_miss = Some(value),
                Some(Err(_)) => {
                    debug!(input, "Failed to parse simulate misses");

                    return Ok(());
                }
                None => self.data.n_miss = None,
            },
            "sim_score" => match input.map(str::parse) {
                Some(Ok(value)) => self.data.score = Some(value),
                Some(Err(_)) => {
                    debug!(input, "Failed to parse simulate score");

                    return Ok(());
                }
                None => self.data.score = None,
            },
            "sim_slider_ends" => match input.map(str::parse) {
                Some(Ok(value)) => self.data.n_slider_ends = Some(value),
                Some(Err(_)) => {
                    debug!(input, "Failed to parse simulate slider ends");

                    return Ok(());
                }
                None => self.data.n_slider_ends = None,
            },
            "sim_large_ticks" => match input.map(str::parse) {
                Some(Ok(value)) => self.data.n_large_ticks = Some(value),
                Some(Err(_)) => {
                    debug!(input, "Failed to parse simulate large ticks");

                    return Ok(());
                }
                None => self.data.n_large_ticks = None,
            },
            "sim_attrs" => {
                self.data.attrs.ar = parse_attr(&*modal, "sim_ar");
                self.data.attrs.cs = parse_attr(&*modal, "sim_cs");
                self.data.attrs.hp = parse_attr(&*modal, "sim_hp");
                self.data.attrs.od = parse_attr(&*modal, "sim_od");
            }
            "sim_speed_adjustments" => {
                self.data.clock_rate = parse_attr(&*modal, "sim_clock_rate");
                self.data.bpm = parse_attr(&*modal, "sim_bpm");
            }
            other => warn!(name = %other, ?modal, "Unknown simulate modal"),
        }

        if let Err(err) = modal.defer().await {
            warn!(?err, "Failed to defer modal");
        }

        Ok(())
    }
}

impl SimulateComponents {
    pub fn new(map: SimulateMap, data: SimulateData, msg_owner: Id<UserMarker>) -> Self {
        Self {
            map,
            data,
            msg_owner,
            defer: true,
        }
    }

    async fn handle_topold_menu(
        &mut self,
        component: &mut InteractionComponent,
    ) -> ComponentResult {
        let Some(version) = component.data.values.first() else {
            return ComponentResult::Err(eyre!("Missing simulate version"));
        };

        let Some(version) = TopOldVersion::from_menu_str(version) else {
            return ComponentResult::Err(eyre!("Unknown TopOldVersion `{version}`"));
        };

        if let Err(err) = component.defer().await.map_err(Report::new) {
            return ComponentResult::Err(err.wrap_err("Failed to defer component"));
        }

        self.data.version = version;

        ComponentResult::BuildPage
    }
}

fn parse_attr<T: FromStr>(modal: &InteractionModal, component_id: &str) -> Option<T> {
    modal
        .data
        .components
        .iter()
        .find_map(|row| {
            row.components.first().and_then(|component| {
                (component.custom_id == component_id).then(|| {
                    component
                        .value
                        .as_deref()
                        .filter(|value| !value.is_empty())
                        .map(str::parse)
                        .and_then(Result::ok)
                })
            })
        })
        .flatten()
}

pub enum SimulateMap {
    Full(OsuMap),
    Attached(AttachedSimulateMap),
}

impl Debug for SimulateMap {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        match self {
            Self::Full(map) => Debug::fmt(&map.map_id(), f),
            Self::Attached(map) => Debug::fmt(map.filename.as_ref(), f),
        }
    }
}

impl SimulateMap {
    pub fn mode(&self) -> GameMode {
        match self.pp_map().mode {
            Mode::Osu => GameMode::Osu,
            Mode::Taiko => GameMode::Taiko,
            Mode::Catch => GameMode::Catch,
            Mode::Mania => GameMode::Mania,
        }
    }

    pub fn pp_map(&self) -> &Beatmap {
        match self {
            Self::Full(map) => &map.pp_map,
            Self::Attached(map) => &map.pp_map,
        }
    }

    pub fn pp_map_mut(&mut self) -> &mut Beatmap {
        match self {
            Self::Full(map) => &mut map.pp_map,
            Self::Attached(map) => &mut map.pp_map,
        }
    }

    pub fn n_objects(&self) -> u32 {
        self.pp_map().hit_objects.len() as u32
    }

    pub fn bpm(&self) -> f32 {
        match self {
            Self::Full(map) => map.bpm(),
            Self::Attached(map) => map.pp_map.bpm() as f32,
        }
    }

    pub fn map_info(&self, stars: f32, mods: &GameMods, clock_rate: Option<f64>) -> String {
        match self {
            Self::Full(map) => {
                let mut map_info = MapInfo::new(map, stars);

                map_info.mods(mods).clock_rate(clock_rate).to_string()
            }
            Self::Attached(map) => {
                let map = &map.pp_map;
                let bits = mods.bits();

                let mut builder = map.attributes();

                if let Some(clock_rate) = clock_rate.or_else(|| mods.clock_rate()) {
                    builder = builder.clock_rate(clock_rate);
                }

                // Technically probably not necessary since users cannot input
                // DA-specific settings through the discord interface but let's
                // consider the mod regardless.
                for gamemod in mods.iter() {
                    match gamemod {
                        GameMod::DifficultyAdjustOsu(m) => {
                            let DifficultyAdjustOsu {
                                circle_size,
                                approach_rate,
                                drain_rate,
                                overall_difficulty,
                                ..
                            } = m;

                            if let Some(cs) = circle_size {
                                builder = builder.cs(*cs as f32, false);
                            }

                            if let Some(ar) = approach_rate {
                                builder = builder.ar(*ar as f32, false);
                            }

                            if let Some(hp) = drain_rate {
                                builder = builder.hp(*hp as f32, false);
                            }

                            if let Some(od) = overall_difficulty {
                                builder = builder.od(*od as f32, false);
                            }
                        }
                        GameMod::DifficultyAdjustTaiko(m) => {
                            let DifficultyAdjustTaiko {
                                drain_rate,
                                overall_difficulty,
                                ..
                            } = m;

                            if let Some(hp) = drain_rate {
                                builder = builder.hp(*hp as f32, false);
                            }

                            if let Some(od) = overall_difficulty {
                                builder = builder.od(*od as f32, false);
                            }
                        }
                        GameMod::DifficultyAdjustCatch(m) => {
                            let DifficultyAdjustCatch {
                                circle_size,
                                approach_rate,
                                drain_rate,
                                overall_difficulty,
                                ..
                            } = m;

                            if let Some(cs) = circle_size {
                                builder = builder.cs(*cs as f32, false);
                            }

                            if let Some(ar) = approach_rate {
                                builder = builder.ar(*ar as f32, false);
                            }

                            if let Some(hp) = drain_rate {
                                builder = builder.hp(*hp as f32, false);
                            }

                            if let Some(od) = overall_difficulty {
                                builder = builder.od(*od as f32, false);
                            }
                        }
                        GameMod::DifficultyAdjustMania(m) => {
                            let DifficultyAdjustMania {
                                drain_rate,
                                overall_difficulty,
                                ..
                            } = m;

                            if let Some(hp) = drain_rate {
                                builder = builder.hp(*hp as f32, false);
                            }

                            if let Some(od) = overall_difficulty {
                                builder = builder.od(*od as f32, false);
                            }
                        }
                        _ => {}
                    }
                }

                let attrs = builder.mods(bits).build();

                let clock_rate = attrs.clock_rate;

                let start_time = map.hit_objects.first().map_or(0.0, |h| h.start_time);
                let end_time = map.hit_objects.last().map_or(0.0, |h| match &h.kind {
                    HitObjectKind::Circle => h.start_time,
                    // slider end time is not reasonably accessible at this
                    // point so this will have to suffice
                    HitObjectKind::Slider(_) => h.start_time,
                    HitObjectKind::Spinner(Spinner { duration })
                    | HitObjectKind::Hold(HoldNote { duration }) => h.start_time + duration,
                });

                let mut sec_drain = ((end_time - start_time) / 1000.0) as u32;

                let mut bpm = map.bpm() as f32;

                if (clock_rate - 1.0).abs() > f64::EPSILON {
                    let clock_rate = clock_rate as f32;

                    bpm *= clock_rate;
                    sec_drain = (sec_drain as f32 / clock_rate) as u32;
                }

                let (cs_key, cs_value) = if map.mode == Mode::Mania {
                    ("Keys", MapInfo::keys(bits, attrs.cs as f32))
                } else {
                    ("CS", round(attrs.cs as f32))
                };

                format!(
                    "Length: `{len}` BPM: `{bpm}` Objects: `{objs}`\n\
                    {cs_key}: `{cs_value}` AR: `{ar}` OD: `{od}` HP: `{hp}` Stars: `{stars}`",
                    len = SecToMinSec::new(sec_drain),
                    bpm = round(bpm),
                    objs = self.n_objects(),
                    ar = round(attrs.ar as f32),
                    od = round(attrs.od as f32),
                    hp = round(attrs.hp as f32),
                    stars = round(stars),
                )
            }
        }
    }
}

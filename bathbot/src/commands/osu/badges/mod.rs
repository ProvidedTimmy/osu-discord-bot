use std::{borrow::Cow, cmp::Reverse};

use bathbot_macros::{HasName, SlashCommand};
use bathbot_model::OsekaiBadge;
use eyre::Result;
use twilight_interactions::command::{
    AutocompleteValue, CommandModel, CommandOption, CreateCommand, CreateOption,
};
use twilight_model::id::{Id, marker::UserMarker};

use self::{query::*, user::*};
use crate::{
    commands::{DISCORD_OPTION_DESC, DISCORD_OPTION_HELP},
    util::{InteractionCommandExt, interaction::InteractionCommand},
};

mod query;
mod user;

#[derive(CreateCommand, SlashCommand)]
#[command(name = "badges", desc = "Display info about badges")]
#[allow(dead_code)]
pub enum Badges<'a> {
    #[command(name = "query")]
    Query(BadgesQuery),
    #[command(name = "user")]
    User(BadgesUser<'a>),
}

#[derive(CommandModel)]
enum Badges_<'a> {
    #[command(name = "query")]
    Query(BadgesQuery_<'a>),
    #[command(name = "user")]
    User(BadgesUser<'a>),
}

const BADGE_QUERY_DESC: &str = "Display all badges matching the query";

#[derive(CreateCommand)]
#[command(name = "query", desc = BADGE_QUERY_DESC)]
#[allow(dead_code)]
pub struct BadgesQuery {
    #[command(autocomplete = true, desc = "Specify the badge name or acronym")]
    name: String,
    #[command(desc = "Choose how the badges should be ordered")]
    sort: Option<BadgesOrder>,
}

#[derive(CommandModel)]
#[command(autocomplete = true)]
struct BadgesQuery_<'a> {
    name: AutocompleteValue<Cow<'a, str>>,
    sort: Option<BadgesOrder>,
}

const BADGE_USER_DESC: &str = "Display all badges of a user";

#[derive(CommandModel, CreateCommand, HasName)]
#[command(name = "user", desc = BADGE_USER_DESC)]
pub struct BadgesUser<'a> {
    #[command(desc = "Specify a username")]
    name: Option<Cow<'a, str>>,
    #[command(desc = "Choose how the badges should be ordered")]
    sort: Option<BadgesOrder>,
    #[command(desc = DISCORD_OPTION_DESC, help = DISCORD_OPTION_HELP)]
    discord: Option<Id<UserMarker>>,
}

#[derive(CommandOption, CreateOption, Default)]
pub enum BadgesOrder {
    #[option(name = "Alphabetically", value = "alphabet")]
    Alphabet,
    #[option(name = "Date", value = "date")]
    #[default]
    Date,
    #[option(name = "Owner count", value = "owners")]
    Owners,
}

impl BadgesOrder {
    fn apply(self, badges: &mut [OsekaiBadge]) {
        match self {
            Self::Alphabet => badges.sort_unstable_by(|a, b| a.name.cmp(&b.name)),
            Self::Date => badges.sort_unstable_by_key(|badge| Reverse(badge.awarded_at)),
            Self::Owners => badges.sort_unstable_by_key(|badge| Reverse(badge.users.len())),
        }
    }
}

pub async fn slash_badges(mut command: InteractionCommand) -> Result<()> {
    match Badges_::from_interaction(command.input_data())? {
        Badges_::Query(args) => match args.name {
            AutocompleteValue::None => query_autocomplete(&command, String::new()).await,
            AutocompleteValue::Focused(name) => query_autocomplete(&command, name).await,
            AutocompleteValue::Completed(name) => {
                query((&mut command).into(), name, args.sort).await
            }
        },
        Badges_::User(args) => user((&mut command).into(), args).await,
    }
}

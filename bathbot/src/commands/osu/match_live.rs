use std::borrow::Cow;

use bathbot_macros::{SlashCommand, command};
use bathbot_model::command_fields::ThreadChannel;
use bathbot_util::{
    MessageBuilder,
    constants::{
        GENERAL_ISSUE, INVALID_ACTION_FOR_CHANNEL_TYPE, OSU_API_ISSUE, OSU_BASE,
        THREADS_UNAVAILABLE,
    },
    matcher,
};
use eyre::{Report, Result, WrapErr};
use twilight_http::{api_error::ApiError, error::ErrorType};
use twilight_interactions::command::{CommandModel, CreateCommand};
use twilight_model::channel::{ChannelType, thread::AutoArchiveDuration};

use crate::{
    Context,
    core::commands::CommandOrigin,
    matchlive::MatchTrackResult,
    util::{ChannelExt, CheckPermissions, InteractionCommandExt, interaction::InteractionCommand},
};

#[derive(CommandModel, CreateCommand, SlashCommand)]
#[command(
    name = "matchlive",
    desc = "Live track a multiplayer match",
    help = "Similar to what an mp link does, this command will \
    keep a channel up to date about events in a multiplayer match."
)]
#[flags(AUTHORITY)]
pub enum Matchlive<'a> {
    #[command(name = "track")]
    Add(MatchliveAdd<'a>),
    #[command(name = "untrack")]
    Remove(MatchliveRemove<'a>),
}

#[derive(CommandModel, CreateCommand)]
#[command(name = "track", desc = "Start tracking a match")]
pub struct MatchliveAdd<'a> {
    #[command(desc = "Specify a match url or match id")]
    match_url: Cow<'a, str>,
    #[command(desc = "Choose if a new thread should be started")]
    thread: ThreadChannel,
}

#[derive(CommandModel, CreateCommand)]
#[command(name = "untrack", desc = "Untrack a match")]
pub struct MatchliveRemove<'a> {
    #[command(desc = "Specify a match url or match id")]
    match_url: Cow<'a, str>,
}

async fn slash_matchlive(mut command: InteractionCommand) -> Result<()> {
    match Matchlive::from_interaction(command.input_data())? {
        Matchlive::Add(args) => matchlive((&mut command).into(), args).await,
        Matchlive::Remove(args) => matchliveremove((&mut command).into(), Some(args)).await,
    }
}

#[command]
#[desc("Live track a multiplayer match")]
#[help(
    "Live track a multiplayer match in a channel.\n\
    Similar to what an mp link does, I will keep a channel up \
    to date about events in a match.\n\
    Use the `matchliveremove` command to stop tracking the match."
)]
#[usage("[match url / match id]")]
#[examples("58320988", "https://osu.ppy.sh/community/matches/58320988")]
#[alias("mla", "matchliveadd", "mlt", "matchlivetrack")]
#[bucket(MatchLive)]
#[flags(AUTHORITY)]
#[group(AllModes)]
async fn prefix_matchlive(msg: &Message, mut args: Args<'_>) -> Result<()> {
    match args.next() {
        Some(arg) => {
            let args = MatchliveAdd {
                match_url: arg.into(),
                thread: ThreadChannel::Channel,
            };

            matchlive(msg.into(), args).await
        }
        None => {
            let content = "You must specify either a match id or a multiplayer link to a match";
            msg.error(content).await?;

            Ok(())
        }
    }
}

#[command]
#[desc("Untrack a multiplayer match")]
#[help(
    "Untrack a multiplayer match in a channel.\n\
    The match id only has to be specified in case the channel \
    currently live tracks more than one match."
)]
#[usage("[match url / match id]")]
#[examples("58320988", "https://osu.ppy.sh/community/matches/58320988")]
#[alias("mlr")]
#[flags(AUTHORITY)]
#[group(AllModes)]
async fn prefix_matchliveremove(msg: &Message, mut args: Args<'_>) -> Result<()> {
    let args = match args.next() {
        Some(arg) => match parse_match_id(arg) {
            Ok(_) => Some(MatchliveRemove {
                match_url: arg.into(),
            }),
            Err(content) => {
                msg.error(content).await?;

                return Ok(());
            }
        },
        None => None,
    };

    matchliveremove(msg.into(), args).await
}

fn parse_match_id(match_url: &str) -> Result<u32, &'static str> {
    match matcher::get_osu_match_id(match_url) {
        Some(id) => Ok(id),
        None => {
            let content = "Failed to parse match url.\n\
                Be sure to provide either a match id or the multiplayer link to a match";

            Err(content)
        }
    }
}

async fn matchlive(orig: CommandOrigin<'_>, args: MatchliveAdd<'_>) -> Result<()> {
    let MatchliveAdd { match_url, thread } = args;

    let match_id = match parse_match_id(&match_url) {
        Ok(id) => id,
        Err(content) => return orig.error(content).await,
    };

    let mut channel = orig.channel_id();

    if let ThreadChannel::Thread = thread {
        if orig.guild_id().is_none() {
            return orig.error(THREADS_UNAVAILABLE).await;
        }

        if !orig.can_create_thread() {
            let content = "I'm lacking the permission to create public threads";

            return orig.error(content).await;
        }

        let kind = ChannelType::PublicThread;
        let archive_dur = AutoArchiveDuration::Day;
        let thread_name = format!("Live tracking match id {match_id}");

        let create_fut = Context::http()
            .create_thread(channel, &thread_name, kind)
            .auto_archive_duration(archive_dur);

        match create_fut.await {
            Ok(res) => channel = res.model().await?.id,
            Err(err) => {
                let content = match err.kind() {
                    ErrorType::Response {
                        error: ApiError::General(err),
                        ..
                    } => match err.code {
                        INVALID_ACTION_FOR_CHANNEL_TYPE => Some(THREADS_UNAVAILABLE),
                        _ => None,
                    },
                    _ => None,
                };

                match content {
                    Some(content) => return orig.error(content).await,
                    None => {
                        let _ = orig.error(GENERAL_ISSUE).await;
                        let report = Report::new(err).wrap_err("failed to create thread");

                        return Err(report);
                    }
                }
            }
        }
    }

    let content: &str = match Context::add_match_track(channel, match_id).await {
        MatchTrackResult::Added => match orig {
            CommandOrigin::Message { .. } => return Ok(()),
            CommandOrigin::Interaction { command } => {
                Context::interaction()
                    .delete_response(&command.token)
                    .await
                    .wrap_err("Failed to delete response")?;

                return Ok(());
            }
        },
        MatchTrackResult::Capped => "Channels can track at most three games at a time",
        MatchTrackResult::Duplicate => "That match is already being tracking in this channel",
        MatchTrackResult::Error => OSU_API_ISSUE,
        MatchTrackResult::NotFound => "The osu!api returned a 404 indicating an invalid match id",
        MatchTrackResult::Private => "The match can't be tracked because it is private",
    };

    orig.error(content).await
}

async fn matchliveremove(orig: CommandOrigin<'_>, args: Option<MatchliveRemove<'_>>) -> Result<()> {
    let channel = orig.channel_id();

    let match_id = match args.map(|args| parse_match_id(&args.match_url)) {
        Some(Ok(id)) => id,
        Some(Err(content)) => return orig.error(content).await,
        None => match Context::tracks_single_match(channel).await {
            Some(id) => id,
            None => {
                let content = "The channel does not track exactly one match \
                    and the match id could not be parsed from the first argument.\n\
                    Try specifying the match id as first argument.";

                return orig.error(content).await;
            }
        },
    };

    if Context::remove_match_track(channel, match_id).await {
        let content =
            format!("Stopped live tracking [the match]({OSU_BASE}community/matches/{match_id})",);

        let builder = MessageBuilder::new().embed(content);
        orig.create_message(builder).await?;

        Ok(())
    } else {
        let content = "The match wasn't tracked in this channel";

        orig.error(content).await
    }
}

use std::collections::BTreeMap;

use bathbot_macros::command;
use bathbot_util::{
    MessageBuilder,
    constants::{AVATAR_URL, GENERAL_ISSUE, OSEKAI_ISSUE},
    matcher,
};
use eyre::{Report, Result};
use rkyv::{
    rancor::{Panic, ResultExt},
    rend::u32_le,
};
use rosu_v2::{model::GameMode, prelude::OsuError, request::UserId};
use twilight_model::guild::Permissions;

use super::BadgesUser;
use crate::{
    active::{ActiveMessages, impls::BadgesPagination},
    commands::osu::{badges::BADGE_USER_DESC, require_link, user_not_found},
    core::{
        Context,
        commands::{CommandOrigin, prefix::Args},
    },
    manager::redis::osu::{UserArgs, UserArgsError},
    util::osu::get_combined_thumbnail,
};

impl<'m> BadgesUser<'m> {
    fn args(args: Args<'m>) -> Self {
        let mut name = None;
        let mut discord = None;

        for arg in args {
            if let Some(id) = matcher::get_mention_user(arg) {
                discord = Some(id);
            } else {
                name = Some(arg.into());
            }
        }

        Self {
            name,
            discord,
            sort: None,
        }
    }
}

#[command]
#[desc(BADGE_USER_DESC)]
#[usage("[username]")]
#[examples("bubbleman")]
#[aliases("userbadges", "userbadge", "badgeuser")]
#[group(AllModes)]
async fn prefix_badgesuser(
    msg: &Message,
    args: Args<'_>,
    perms: Option<Permissions>,
) -> Result<()> {
    let orig = CommandOrigin::from_msg(msg, perms);
    let args = BadgesUser::args(args);

    user(orig, args).await
}

pub(super) async fn user(orig: CommandOrigin<'_>, args: BadgesUser<'_>) -> Result<()> {
    let owner = orig.user_id()?;

    let user_id = match user_id!(orig, args) {
        Some(user_id) => user_id,
        None => match Context::user_config().osu_id(owner).await {
            Ok(Some(id)) => UserId::Id(id),
            Ok(None) => return require_link(&orig).await,
            Err(err) => {
                let _ = orig.error(GENERAL_ISSUE).await;

                return Err(err.wrap_err("failed to get user id"));
            }
        },
    };

    let user_args_fut = UserArgs::rosu_id(&user_id, GameMode::Osu);
    let badges_fut = Context::redis().badges();

    let (user_args, badges_res) = tokio::join!(user_args_fut, badges_fut);

    let (user_id_raw, user_id) = match user_args {
        UserArgs::Args(args) => (args.user_id, user_id),
        UserArgs::User { user, .. } => (
            user.user_id.to_native(),
            UserId::Name(user.username.as_str().into()),
        ),
        UserArgs::Err(UserArgsError::Osu(OsuError::NotFound)) => {
            let content = user_not_found(user_id).await;

            return orig.error(content).await;
        }
        UserArgs::Err(err) => {
            let _ = orig.error(GENERAL_ISSUE).await;
            let err = Report::new(err).wrap_err("Failed to get user");

            return Err(err);
        }
    };

    let badges = match badges_res {
        Ok(badges) => badges,
        Err(err) => {
            let _ = orig.error(OSEKAI_ISSUE).await;

            return Err(Report::new(err).wrap_err("Failed to get badges"));
        }
    };

    let mut badges: Vec<_> = badges
        .iter()
        .filter(|badge| badge.users.contains(&u32_le::from_native(user_id_raw)))
        .map(|badge| rkyv::api::deserialize_using::<_, _, Panic>(badge, &mut ()).always_ok())
        .collect();

    args.sort.unwrap_or_default().apply(&mut badges);

    let owners = if let Some(badge) = badges.first() {
        let owners_fut = Context::client().get_osekai_badge_owners(badge.badge_id);

        match owners_fut.await {
            Ok(owners) => owners,
            Err(err) => {
                let _ = orig.error(OSEKAI_ISSUE).await;
                let wrap = format!("Failed to get badge owners for badge id {}", badge.badge_id);

                return Err(err.wrap_err(wrap));
            }
        }
    } else {
        let user_id = match user_id {
            UserId::Id(user_id) => match Context::osu_user().name(user_id).await {
                Ok(Some(name)) => UserId::Name(name),
                Ok(None) => UserId::Id(user_id),
                Err(err) => {
                    warn!("{err:?}");

                    UserId::Id(user_id)
                }
            },
            user_id @ UserId::Name(_) => user_id,
        };

        let content = match user_id {
            UserId::Id(user_id) => format!("User with id {user_id} has no badges :("),
            UserId::Name(name) => format!("User `{name}` has no badges :("),
        };

        let builder = MessageBuilder::new().embed(content);
        orig.create_message(builder).await?;

        return Ok(());
    };

    let urls: Vec<_> = owners
        .iter()
        .map(|owner| format!("{AVATAR_URL}{}", owner.user_id).into_boxed_str())
        .collect();

    let urls = urls.iter().map(Box::as_ref);

    let bytes = if badges.len() == 1 {
        match get_combined_thumbnail(urls, owners.len() as u32, Some(1024)).await {
            Ok(bytes) => Some(bytes),
            Err(err) => {
                warn!(?err, "Failed to combine avatars");

                None
            }
        }
    } else {
        None
    };

    let mut owners_map = BTreeMap::new();
    owners_map.insert(0, owners.into_boxed_slice());

    let pagination = BadgesPagination::builder()
        .badges(badges.into_boxed_slice())
        .owners(owners_map)
        .msg_owner(owner)
        .build();

    ActiveMessages::builder(pagination)
        .start_by_update(true)
        .attachment(bytes.map(|bytes| ("badge_owners.png".to_owned(), bytes)))
        .begin(orig)
        .await
}

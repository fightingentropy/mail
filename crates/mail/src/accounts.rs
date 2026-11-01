//! Signing accounts in, connecting to them, and keeping their profile
//! photos. The command line and the window share these.

use std::fs;

use mail_core::{
    auth::{self, OAuthClient, keychain},
    gmail::{self, GmailApi, GmailClient},
    paths::Paths,
    store::Store,
};

/// Runs Google sign-in in the browser and remembers the account: in the
/// database, its refresh token in the Keychain, and its photo. Signing in to
/// an account that is already here renews its token and photo. Returns the
/// account's address.
pub fn sign_in(
    paths: &Paths,
    client: OAuthClient,
    open_browser: impl FnOnce(&str) -> anyhow::Result<()>,
) -> anyhow::Result<String> {
    let sign_in = auth::sign_in(&gmail::http_agent(), &client, open_browser)?;
    let gmail = GmailClient::new(client, sign_in.refresh_token.clone(), Some(sign_in.access));
    let email = gmail.profile()?.email_address;
    Store::open(&paths.database())?.add_account(&email)?;
    keychain::save(&email, &sign_in.refresh_token)?;
    // The photo is a nicety: initials stand in when it can't be fetched.
    let _ = save_photo(paths, &gmail, &email);
    Ok(email)
}

/// A Gmail client for one signed-in account.
pub fn client(paths: &Paths, email: &str) -> anyhow::Result<GmailClient> {
    Ok(GmailClient::new(
        OAuthClient::load(&paths.oauth_client())?,
        keychain::load(email)?,
        None,
    ))
}

/// Fetches and saves an account's profile photo. Returns false when Google
/// gives none, as for a sign-in from before Mail asked to see it.
pub fn save_photo(paths: &Paths, gmail: &GmailClient, email: &str) -> anyhow::Result<bool> {
    let Some(bytes) = gmail.profile_photo()? else {
        return Ok(false);
    };
    let path = paths.photo(email);
    if let Some(folder) = path.parent() {
        fs::create_dir_all(folder)?;
    }
    fs::write(&path, bytes)?;
    Ok(true)
}

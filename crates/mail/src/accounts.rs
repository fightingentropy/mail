//! Signing accounts in, Gmail or iCloud, connecting to them, and keeping
//! their profile photos. The command line and the window share these.

use std::{fs, io::Cursor, path::Path, process::Command};

use anyhow::Context;
use image::{DynamicImage, ImageDecoder, ImageReader, imageops::FilterType};
use mail_core::{
    auth::{self, OAuthClient, keychain},
    gmail::{self, GmailApi, GmailClient},
    icloud::{IcloudClient, Login},
    paths::Paths,
    store::Store,
};

/// A Gmail or iCloud account, through the calls both answer.
pub type Client = Box<dyn GmailApi + Send>;

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

/// Checks an iCloud address and app-specific password with iCloud and
/// remembers the account, its login in the Keychain. Returns the address.
pub fn sign_in_icloud(paths: &Paths, address: &str, password: &str) -> anyhow::Result<String> {
    let (_, login) = IcloudClient::sign_in(address, password)?;
    let address = address.trim();
    Store::open(&paths.database())?.add_account(address)?;
    keychain::save(address, &login.to_secret())?;
    Ok(address.to_owned())
}

/// A client for one signed-in account. The Keychain says which kind: an
/// iCloud login, or a Google refresh token.
pub fn client(paths: &Paths, email: &str) -> anyhow::Result<Client> {
    let secret = keychain::load(email)?;
    if let Some(login) = Login::from_secret(&secret) {
        return Ok(Box::new(IcloudClient::new(email, login)));
    }
    Ok(Box::new(GmailClient::new(
        OAuthClient::load(&paths.oauth_client())?,
        secret,
        None,
    )))
}

/// Fetches and saves an account's profile photo. Returns false when there
/// is none: iCloud has none to give, and Google none for a sign-in from
/// before Mail asked to see it.
pub fn save_photo(paths: &Paths, gmail: &dyn GmailApi, email: &str) -> anyhow::Result<bool> {
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

/// The account's photo: the one you chose, else Google's.
pub fn read_photo(paths: &Paths, email: &str) -> Option<DynamicImage> {
    [paths.chosen_photo(email), paths.photo(email)]
        .iter()
        .find_map(|path| decode(&fs::read(path).ok()?).ok())
}

/// Makes `source` the account's photo. It is cropped to a square and saved
/// as a small PNG, which also drops metadata such as where it was taken.
/// HEIC and other formats Mail can't read go through macOS `sips` first.
pub fn choose_photo(paths: &Paths, email: &str, source: &Path) -> anyhow::Result<()> {
    let bytes = fs::read(source).with_context(|| format!("Could not read {}", source.display()))?;
    let photo = match decode(&bytes) {
        Ok(photo) => photo,
        Err(_) => decode(&converted_by_sips(source)?).context("Mail can't read that image")?,
    };
    let square = photo.resize_to_fill(256, 256, FilterType::Lanczos3);
    let path = paths.chosen_photo(email);
    if let Some(folder) = path.parent() {
        fs::create_dir_all(folder)?;
    }
    square
        .to_rgb8()
        .save_with_format(&path, image::ImageFormat::Png)?;
    Ok(())
}

/// Decodes an image the right way up: phones store photos sideways with an
/// orientation tag.
fn decode(bytes: &[u8]) -> anyhow::Result<DynamicImage> {
    let mut decoder = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()?
        .into_decoder()?;
    let orientation = decoder.orientation()?;
    let mut photo = DynamicImage::from_decoder(decoder)?;
    photo.apply_orientation(orientation);
    Ok(photo)
}

fn converted_by_sips(source: &Path) -> anyhow::Result<Vec<u8>> {
    let folder = std::env::temp_dir().join(format!("mail-photo-{}", std::process::id()));
    fs::create_dir_all(&folder)?;
    let out = folder.join("photo.jpg");
    let status = Command::new("sips")
        .args(["-s", "format", "jpeg"])
        .arg(source)
        .arg("--out")
        .arg(&out)
        .output()?
        .status;
    anyhow::ensure!(status.success(), "Mail can't read that image");
    let bytes = fs::read(&out)?;
    let _ = fs::remove_dir_all(&folder);
    Ok(bytes)
}

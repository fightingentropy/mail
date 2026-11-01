# Connecting Mail to Gmail

Mail signs in with your own Google Cloud OAuth client. Nobody else's server
sits between Mail and Gmail. You set this up once; it takes about ten minutes.

You do every step yourself, in your browser. Mail never asks for your Google
password.

## 1. Create the Google Cloud project

1. Open [console.cloud.google.com](https://console.cloud.google.com/) and
   create a project, for example **Mail**.
2. Go to **APIs & Services → Library**, find **Gmail API** and click
   **Enable**.

## 2. Configure the consent screen

Open **Google Auth Platform** (APIs & Services → OAuth consent screen) and
click **Get started**.

1. **App information:** a name such as **Ana's Mail**. Google rejects plain
   "Mail" as an app name. Use your own address as the support email.
2. **Audience:** **External**.
3. **Contact information:** your address. Agree to the policy and create.
4. **Data Access → Add or remove scopes:** paste
   `https://www.googleapis.com/auth/gmail.modify` under *Manually add scopes*,
   click **Add to table**, then **Update** and **Save**.
5. **Branding → App domain:** Google won't let you publish without a home
   page and a privacy policy link. Your fork on GitHub works for both, for
   example `https://github.com/you/mail` and
   `https://github.com/you/mail#privacy`. Add `github.com` under
   **Authorized domains** and save.

## 3. Create the Desktop client

1. **Clients → Create client**, application type **Desktop app**, name
   **Mail for Mac**.
2. Click **Download JSON** before closing the dialog: Google shows the client
   secret only once. The file is named like `client_secret_….json`.

Keep the file out of git. The repository's `.gitignore` already ignores
`client_secret*.json`.

## 4. Publish the app

**Audience → Publish app → Confirm.** The status becomes *In production*
and the app stays *unverified*.

This matters because apps left in *Testing* get Gmail tokens that expire
after 7 days. Google doesn't require verification for personal-use apps with
fewer than 100 users. Don't submit the app for verification.

## 5. Sign in

```bash
mail auth --client ~/Downloads/client_secret_XXXX.json
```

Your browser opens Google's sign-in:

1. Choose your account.
2. Google shows **"Google hasn't verified this app"**. That is expected for
   your own unverified app. Click **Advanced → Go to <your app name> (unsafe)**.
3. Allow Mail to read, compose and organize your email.
4. The tab says *Mail is signed in*. Back in the terminal:

```bash
mail sync
mail search invoice
```

You can delete the downloaded JSON afterwards. Mail keeps its own copy in
`~/Library/Application Support/org.erlin.mail/`.

## Signing out

- Revoke access at [myaccount.google.com/connections](https://myaccount.google.com/connections).
- Remove the token from the Keychain:

```bash
security delete-generic-password -s org.erlin.mail -a you@gmail.com
```

## When Mail asks you to sign in again

Google revokes the token when you change your Google password, after six
months without use, or when you remove access yourself. If you skipped step 4
it also expires after 7 days. Run `mail auth` again; no `--client` is needed
the second time.

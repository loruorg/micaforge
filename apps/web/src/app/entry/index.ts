/**
 * ENTRY: the way in.
 *
 * Auth, the site chooser, first-run onboarding and the public share view. The
 * router imports the screens from here; nothing outside this directory needs to
 * read any other file in it.
 *
 * Routes this area expects to be mounted at (see ENTRY_PATHS, which every
 * screen navigates by, so the two never drift apart):
 *
 *   /app/login                    LoginScreen           (sign in, or create an account, by code)
 *   /app/invitation               InvitationScreen      (token as :token or ?token=)
 *   /app                          SiteChooserScreen
 *   /app/new                      AddSiteScreen
 *   /app/:site/onboarding         OnboardingScreen
 *   /share/:slug                  ShareScreen
 *
 * There are no passwords, so there is no signup, recovery or reset screen: the
 * router sends the old addresses for those to sign-in.
 *
 * The two auth screens and ShareScreen draw their own full-page frame, so they
 * are mounted OUTSIDE AppShell. SiteChooserScreen, AddSiteScreen and
 * OnboardingScreen render page content only and belong inside it.
 *
 * Every screen in here reads through react-query, so /share/:slug has to sit
 * inside the QueryClientProvider along with everything else.
 */
import "../../styles/app/entry.css";

export { AuthFrame, ENTRY_PATHS, Field, PasswordField, Select, FormError, FormOk, Waiting, useInstanceConfig, fieldError, formError } from "./AuthFrame";
export type { AuthFrameProps, FieldProps, InstanceConfig } from "./AuthFrame";

export { CodeSignIn, CODE_LENGTH, codeDigits } from "./CodeSignIn";
export type { CodeSignInProps, CodeStep, SignedIn } from "./CodeSignIn";
export { LoginScreen } from "./LoginScreen";
export { InvitationScreen } from "./InvitationScreen";
export { SiteChooserScreen } from "./SiteChooserScreen";
export { AddSiteScreen, readDomain } from "./AddSiteScreen";
export type { AddSiteScreenProps, DomainReading } from "./AddSiteScreen";
export { OnboardingScreen } from "./OnboardingScreen";
export type { OnboardingScreenProps } from "./OnboardingScreen";
export { ShareScreen } from "./ShareScreen";
export type { ShareMeta } from "./ShareScreen";

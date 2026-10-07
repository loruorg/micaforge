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
 *   /app/login                    LoginScreen
 *   /app/signup                   SignupScreen
 *   /app/forgot-password          ForgotPasswordScreen
 *   /app/reset-password           ResetPasswordScreen   (token as :token or ?token=)
 *   /app/invitation               InvitationScreen      (token as :token or ?token=)
 *   /app                          SiteChooserScreen
 *   /app/new                      AddSiteScreen
 *   /app/:site/onboarding         OnboardingScreen
 *   /share/:slug                  ShareScreen
 *
 * The five auth screens and ShareScreen draw their own full-page frame, so they
 * are mounted OUTSIDE AppShell. SiteChooserScreen, AddSiteScreen and
 * OnboardingScreen render page content only and belong inside it.
 *
 * Every screen in here reads through react-query, so /share/:slug has to sit
 * inside the QueryClientProvider along with everything else.
 */
import "../../styles/app/entry.css";

export { AuthFrame, ENTRY_PATHS, Field, PasswordField, Select, FormError, FormOk, Waiting, useInstanceConfig, fieldError, formError } from "./AuthFrame";
export type { AuthFrameProps, FieldProps, InstanceConfig } from "./AuthFrame";

export { LoginScreen } from "./LoginScreen";
export { SignupScreen } from "./SignupScreen";
export { ForgotPasswordScreen } from "./ForgotPasswordScreen";
export { ResetPasswordScreen } from "./ResetPasswordScreen";
export { InvitationScreen } from "./InvitationScreen";
export { SiteChooserScreen } from "./SiteChooserScreen";
export { AddSiteScreen, readDomain } from "./AddSiteScreen";
export type { AddSiteScreenProps, DomainReading } from "./AddSiteScreen";
export { OnboardingScreen } from "./OnboardingScreen";
export type { OnboardingScreenProps } from "./OnboardingScreen";
export { ShareScreen } from "./ShareScreen";
export type { ShareMeta } from "./ShareScreen";

// The script-tag tracker, called through the global.
window.micaforge("event", "newsletter_signup", { source: "footer" });

export function openModal() {
  window.micaforge.track("newsletter_signup", { source: "modal" });
  const widget = `<button data-track="widget_open" data-track-size="small">Open</button>`;
  return widget;
}

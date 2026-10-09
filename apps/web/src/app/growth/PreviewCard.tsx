/**
 * The card a chat app draws when someone pastes a short link.
 *
 * When a chat app meets a link, it fetches the address and reads the Open
 * Graph tags; the short-link server answers that fetch with the title,
 * description and image set here. This card shows the operator roughly what
 * will appear, while they type.
 *
 * It is drawn in neutral tokens on purpose: it is a picture of somebody else's
 * interface, so it takes none of this product's colours. The host it prints is
 * the short link's own host, because that is the address the chat app was
 * given.
 *
 * The image is not requested until the field holds an https address and the
 * typing has paused. An http address would be blocked as mixed content in most
 * chat apps anyway, and fetching on every keystroke would send a request to a
 * stranger's server for each half-typed URL.
 */
import { useState } from "react";

import { useDebounced } from "./controls";
import { isHttpsUrl } from "./model";

export function PreviewCard({
  title,
  description,
  imageUrl,
  host,
}: {
  title: string;
  description: string;
  imageUrl: string;
  host: string;
}) {
  const typed = imageUrl.trim();
  const settled = useDebounced(typed, 450);
  const src = settled && isHttpsUrl(settled) ? settled : null;
  const [failed, setFailed] = useState<string | null>(null);
  const showImage = src != null && failed !== src;

  return (
    <div className="mf-gr-preview">
      <figure className="mf-gr-card" aria-label="How a chat app shows this link">
        {showImage ? (
          <img
            key={src}
            className="mf-gr-card__img"
            src={src}
            alt=""
            loading="lazy"
            decoding="async"
            referrerPolicy="no-referrer"
            onError={() => setFailed(src)}
          />
        ) : null}
        <figcaption className="mf-gr-card__text">
          <span className="mf-gr-card__host">{host}</span>
          <span className={`mf-gr-card__title${title.trim() ? "" : " is-missing"}`}>
            {title.trim() || "No title set"}
          </span>
          {description.trim() ? <span className="mf-gr-card__desc">{description.trim()}</span> : null}
        </figcaption>
      </figure>
      {typed && !isHttpsUrl(typed) ? (
        <p className="mf-gr-preview__note">The image shows here when the address starts with https://.</p>
      ) : null}
      {src != null && failed === src ? (
        <p className="mf-gr-preview__note" data-tone="warn">
          The image at this address did not load. Chat apps will show the card without it.
        </p>
      ) : null}
    </div>
  );
}

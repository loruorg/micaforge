// A function that happens to be called `track`, and is not the SDK's.
function track(parcel: string): string {
  return `tracking ${parcel}`;
}

export const status = track("not_an_event");

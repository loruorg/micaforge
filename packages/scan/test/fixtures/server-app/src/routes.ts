import { micaforge } from "./server";

export function refund(id: string) {
  micaforge.track({ name: "refund_issued", props: { order: id } });
}

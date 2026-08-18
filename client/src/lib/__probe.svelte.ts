export class Probe {
  #n = $state(0);
  get n() { return this.#n; }
  inc() { this.#n++; }
}

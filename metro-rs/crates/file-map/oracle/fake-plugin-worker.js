// A plugin worker for the getWorkerInput oracle: it records that it ran.
'use strict';

module.exports = class FakePluginWorker {
  #id;
  constructor({id}) {
    this.#id = id;
  }
  processFile() {
    return 'ran-' + this.#id;
  }
};

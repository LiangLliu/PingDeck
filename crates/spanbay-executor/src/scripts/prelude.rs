/// JavaScript prelude: installs the `pm` and `console` globals on top of the `__pd` host bridge. It
/// runs in a brand-new Boa context for every script, so nothing a script does outlives it.
pub const PRELUDE: &str = r#"
(function () {
  // Intrinsics the host plumbing depends on are captured before any user code runs, so a script that
  // monkey-patches JSON or __pd cannot break the pm.* bridge.
  const stringify = JSON.stringify;
  const jsonParse = JSON.parse;
  const pd = __pd;
  function bridge(op, payload) {
    return pd(op, stringify(payload === undefined ? null : payload));
  }
  function parse(raw) { return jsonParse(raw); }
  function fmt(value) {
    if (typeof value === "string") { return value; }
    try { return stringify(value); } catch (error) { return String(value); }
  }
  function typeTag(value) { return Object.prototype.toString.call(value).slice(8, -1); }
  // chai's type names: `null`, `array`, `regexp`, `date`, `map`, `set`, `promise`, `error`, …
  function typeName(value) {
    if (value === null) { return "null"; }
    const type = typeof value;
    if (type !== "object") { return type; }
    return typeTag(value).toLowerCase();
  }
  // chai's deep-eql: structural, key order ignored; NaN equals NaN, -0 and +0 differ, a missing
  // key differs from one holding `undefined`, cycles compare by the pair already being compared.
  function deepEqual(left, right, seen) {
    if (left === right) { return left !== 0 || 1 / left === 1 / right; }
    if (left !== left && right !== right) { return true; }
    if (left === null || right === null || typeof left !== "object" || typeof right !== "object") { return false; }
    const tag = typeTag(left);
    if (tag !== typeTag(right)) { return false; }
    const pairs = seen || [];
    for (const pair of pairs) {
      if (pair[0] === left && pair[1] === right) { return true; }
    }
    pairs.push([left, right]);
    switch (tag) {
      case "Boolean": case "Number": case "String":
        return deepEqual(left.valueOf(), right.valueOf(), pairs);
      case "Date": {
        const a = left.getTime(), b = right.getTime();
        return a === b || (a !== a && b !== b);
      }
      case "RegExp": return String(left) === String(right);
      case "Error": case "Promise": case "WeakMap": case "WeakSet": return false;
      case "Map": {
        if (left.size !== right.size) { return false; }
        for (const entry of left) {
          if (!right.has(entry[0]) || !deepEqual(entry[1], right.get(entry[0]), pairs)) { return false; }
        }
        return true;
      }
      case "Set": {
        if (left.size !== right.size) { return false; }
        for (const item of left) {
          if (right.has(item)) { continue; }
          let found = false;
          for (const other of right) { if (deepEqual(item, other, pairs)) { found = true; break; } }
          if (!found) { return false; }
        }
        return true;
      }
    }
    if (Array.isArray(left) || ArrayBuffer.isView(left)) {
      if (left.length !== right.length) { return false; }
      for (let i = 0; i < left.length; i++) {
        if (!deepEqual(left[i], right[i], pairs)) { return false; }
      }
      return true;
    }
    const leftKeys = Object.keys(left).concat(Object.getOwnPropertySymbols(left).filter((key) => Object.prototype.propertyIsEnumerable.call(left, key)));
    const rightKeys = Object.keys(right).concat(Object.getOwnPropertySymbols(right).filter((key) => Object.prototype.propertyIsEnumerable.call(right, key)));
    if (leftKeys.length !== rightKeys.length) { return false; }
    for (const key of leftKeys) {
      if (!Object.prototype.propertyIsEnumerable.call(right, key) || !deepEqual(left[key], right[key], pairs)) { return false; }
    }
    return true;
  }
  function AssertionError(message) {
    this.name = "AssertionError";
    this.message = message;
  }
  AssertionError.prototype = Object.create(Error.prototype);
  AssertionError.prototype.constructor = AssertionError;
  function fail(message) { throw new AssertionError(message); }

  // Names that are looked up on any object by the runtime or by tooling (a returned value is
  // probed for `then`, `JSON.stringify` asks for `toJSON`); they read as absent, not as a typo.
  const PROBED = { then: true, toJSON: true, inspect: true, nodeType: true, asymmetricMatch: true, $$typeof: true, constructor: true };
  // An unknown assertion word throws instead of reading `undefined` and passing: chai does the
  // same, so a typo (`to.be.sucess`) or an unsupported matcher fails the test.
  function strict(target, what) {
    return new Proxy(target, {
      get(object, property, receiver) {
        if (typeof property === "symbol" || property in object) { return Reflect.get(object, property, receiver); }
        if (Object.prototype.hasOwnProperty.call(PROBED, property)) { return undefined; }
        fail("Invalid " + what + " property: " + String(property));
      }
    });
  }
  // A chai "chainable method": callable (`.include(x)`), and a chain whose next words see
  // `flagged` (`.include.members([...])`, `.length.above(2)`, `.an.instanceof(Array)`).
  function chainable(method, flagged) {
    return new Proxy(() => {}, {
      apply(target, self, args) { return method.apply(undefined, args); },
      get(target, property) {
        if (typeof property === "symbol") { return undefined; }
        return flagged[property];
      }
    });
  }
  function nested(value, path) {
    let current = value;
    for (const segment of String(path).replace(/\[(\w+)\]/g, ".$1").split(".")) {
      if (current == null || !(segment in Object(current))) { return { has: false, value: undefined }; }
      current = current[segment];
    }
    return { has: true, value: current };
  }

  class Assertion {
    constructor(actual, flags) {
      this._actual = actual;
      this._flags = flags || {};
      return strict(this, "assertion");
    }
    _with(extra) { return new Assertion(this._actual, Object.assign({}, this._flags, extra)); }
    get _negate() { return !!this._flags.negate; }
    get _deep() { return !!this._flags.deep; }
    get to() { return this; }
    get be() { return this; }
    get been() { return this; }
    get is() { return this; }
    get that() { return this; }
    get which() { return this; }
    get and() { return this; }
    get has() { return this; }
    get have() { return this; }
    get with() { return this; }
    get at() { return this; }
    get of() { return this; }
    get same() { return this; }
    get but() { return this; }
    get does() { return this; }
    get still() { return this; }
    get also() { return this; }
    get itself() { return this._with({ itself: true }); }
    get not() { return this._with({ negate: !this._negate }); }
    get deep() { return this._with({ deep: true }); }
    get own() { return this._with({ own: true }); }
    get nested() { return this._with({ nested: true }); }
    get ordered() { return this._with({ ordered: true }); }
    get any() { return this._with({ any: true, all: false }); }
    get all() { return this._with({ all: true, any: false }); }
    get ok() { return this._check(!!this._actual, "expected a truthy value"); }
    get true() { return this._check(this._actual === true, "expected true"); }
    get false() { return this._check(this._actual === false, "expected false"); }
    get null() { return this._check(this._actual === null, "expected null"); }
    get undefined() { return this._check(this._actual === undefined, "expected undefined"); }
    get NaN() { return this._check(typeof this._actual === "number" && this._actual !== this._actual, "expected NaN"); }
    get exist() { return this._check(this._actual != null, "expected a value that exists (not null or undefined)"); }
    get exists() { return this.exist; }
    get finite() { return this._check(typeof this._actual === "number" && isFinite(this._actual), "expected a finite number"); }
    get extensible() { return this._check(Object.isExtensible(Object(this._actual)) && this._actual === Object(this._actual), "expected an extensible object"); }
    get sealed() { return this._check(Object.isSealed(this._actual), "expected a sealed object"); }
    get frozen() { return this._check(Object.isFrozen(this._actual), "expected a frozen object"); }
    get empty() {
      const value = this._actual;
      let empty;
      if (typeof value === "string" || Array.isArray(value) || ArrayBuffer.isView(value)) { empty = value.length === 0; }
      else if (value instanceof Map || value instanceof Set) { empty = value.size === 0; }
      else if (value !== null && typeof value === "object") { empty = Object.keys(value).length === 0; }
      else { fail("expected a string, array, map, set or object to check for emptiness — got " + fmt(value)); }
      return this._check(empty, "expected an empty value");
    }
    _check(passed, message, expected) {
      const ok = this._negate ? !passed : passed;
      if (!ok) {
        const actual = fmt(this._actual);
        const suffix = expected === undefined ? "" : " " + fmt(expected);
        fail(message + (this._negate ? " (negated)" : "") + " — got " + actual + suffix);
      }
      return this;
    }
    _same(left, right) { return this._deep ? deepEqual(left, right) : left === right; }
    _subject(label) {
      if (!this._flags.length) { return this._actual; }
      const value = this._actual;
      if (value instanceof Map || value instanceof Set) { return value.size; }
      if (value == null || typeof value.length !== "number") { fail("expected a value with a length for " + label + " — got " + fmt(value)); }
      return value.length;
    }
    equal(expected) {
      return this._check(this._same(this._actual, expected), this._deep ? "expected values to deeply equal" : "expected values to be equal", expected);
    }
    equals(expected) { return this.equal(expected); }
    eq(expected) { return this.equal(expected); }
    eql(expected) {
      return this._check(deepEqual(this._actual, expected), "expected values to deeply equal", expected);
    }
    eqls(expected) { return this.eql(expected); }
    _type(type) {
      const actual = typeName(this._actual);
      return this._check(actual === String(type).toLowerCase(), "expected type " + type, actual);
    }
    get a() { return chainable((type) => this._type(type), this); }
    get an() { return this.a; }
    _include(expected) {
      const actual = this._actual;
      let passed;
      if (typeof actual === "string") {
        passed = actual.includes(String(expected));
      } else if (Array.isArray(actual)) {
        passed = actual.some((item) => this._same(item, expected));
      } else if (actual instanceof Set) {
        passed = Array.from(actual).some((item) => this._same(item, expected));
      } else if (actual instanceof Map) {
        passed = Array.from(actual.values()).some((item) => this._same(item, expected));
      } else if (actual !== null && typeof actual === "object") {
        if (expected === null || typeof expected !== "object") {
          fail("expected an object of properties to look for in an object — got " + fmt(expected));
        }
        passed = Object.keys(expected).every((key) => {
          const has = this._flags.own ? Object.prototype.hasOwnProperty.call(actual, key) : key in actual;
          return has && this._same(actual[key], expected[key]);
        });
      } else {
        passed = false;
      }
      return this._check(passed, "expected value to include", expected);
    }
    get include() { return chainable((expected) => this._include(expected), this._with({ contains: true })); }
    get includes() { return this.include; }
    get contain() { return this.include; }
    get contains() { return this.include; }
    property(name, expected) {
      let has, value;
      if (this._flags.nested) {
        const found = nested(this._actual, name);
        has = found.has; value = found.value;
      } else {
        has = this._actual != null && (this._flags.own ? Object.prototype.hasOwnProperty.call(Object(this._actual), name) : name in Object(this._actual));
        value = has ? this._actual[name] : undefined;
      }
      if (arguments.length === 1) {
        this._check(has, "expected property " + name);
        return has && !this._negate ? new Assertion(value, {}) : this;
      }
      this._check(has && this._same(value, expected), "expected property " + name + " to equal", expected);
      return this;
    }
    ownProperty(name, expected) {
      const own = this._with({ own: true });
      return arguments.length === 1 ? own.property(name) : own.property(name, expected);
    }
    haveOwnProperty(name, expected) { return arguments.length === 1 ? this.ownProperty(name) : this.ownProperty(name, expected); }
    nestedProperty(name, expected) {
      const deepPath = this._with({ nested: true });
      return arguments.length === 1 ? deepPath.property(name) : deepPath.property(name, expected);
    }
    _lengthOf(expected) {
      const value = this._actual;
      const size = value instanceof Map || value instanceof Set ? value.size : value != null ? value.length : undefined;
      return this._check(size === expected, "expected length", expected);
    }
    get length() { return chainable((expected) => this._lengthOf(expected), this._with({ length: true })); }
    get lengthOf() { return this.length; }
    above(expected) { return this._check(this._subject("above") > expected, "expected value above", expected); }
    gt(expected) { return this.above(expected); }
    greaterThan(expected) { return this.above(expected); }
    below(expected) { return this._check(this._subject("below") < expected, "expected value below", expected); }
    lt(expected) { return this.below(expected); }
    lessThan(expected) { return this.below(expected); }
    least(expected) { return this._check(this._subject("least") >= expected, "expected value at least", expected); }
    gte(expected) { return this.least(expected); }
    greaterThanOrEqual(expected) { return this.least(expected); }
    most(expected) { return this._check(this._subject("most") <= expected, "expected value at most", expected); }
    lte(expected) { return this.most(expected); }
    lessThanOrEqual(expected) { return this.most(expected); }
    within(low, high) {
      const value = this._subject("within");
      return this._check(value >= low && value <= high, "expected value within", [low, high]);
    }
    closeTo(expected, delta) { return this._check(typeof this._actual === "number" && Math.abs(this._actual - expected) <= delta, "expected value close to", expected); }
    approximately(expected, delta) { return this.closeTo(expected, delta); }
    match(pattern) { return this._check(pattern instanceof RegExp && pattern.test(String(this._actual)), "expected value to match", String(pattern)); }
    matches(pattern) { return this.match(pattern); }
    string(expected) { return this._check(typeof this._actual === "string" && this._actual.includes(expected), "expected string containing", expected); }
    oneOf(values) { return this._check(Array.isArray(values) && values.some((value) => this._same(value, this._actual)), "expected value to be one of", values); }
    satisfy(fn) { return this._check(!!fn(this._actual), "expected value to satisfy predicate"); }
    satisfies(fn) { return this.satisfy(fn); }
    instanceof(ctor) { return this._check(typeof ctor === "function" && this._actual instanceof ctor, "expected instance of", ctor && ctor.name); }
    instanceOf(ctor) { return this.instanceof(ctor); }
    keys() {
      let expected = arguments.length === 1 && Array.isArray(arguments[0]) ? arguments[0]
        : arguments.length === 1 && arguments[0] !== null && typeof arguments[0] === "object" ? Object.keys(arguments[0])
        : Array.prototype.slice.call(arguments);
      expected = expected.map(String);
      const value = this._actual;
      const actual = value instanceof Map ? Array.from(value.keys()).map(String)
        : value !== null && typeof value === "object" ? Object.keys(value) : [];
      let passed;
      if (this._flags.any) { passed = expected.some((key) => actual.includes(key)); }
      else if (this._flags.contains) { passed = expected.every((key) => actual.includes(key)); }
      else { passed = expected.length === actual.length && expected.every((key) => actual.includes(key)); }
      return this._check(passed, "expected keys", expected);
    }
    key() { return this.keys.apply(this, arguments); }
    members(expected) {
      const actual = this._actual;
      if (!Array.isArray(actual) || !Array.isArray(expected)) { fail("expected arrays to compare members — got " + fmt(actual)); }
      const same = (left, right) => this._same(left, right);
      let passed;
      if (this._flags.ordered) {
        passed = (this._flags.contains ? expected.length <= actual.length : expected.length === actual.length)
          && expected.every((item, index) => same(item, actual[index]));
      } else {
        const remaining = actual.slice();
        passed = expected.every((item) => {
          const index = remaining.findIndex((other) => same(item, other));
          if (index === -1) { return false; }
          remaining.splice(index, 1);
          return true;
        }) && (this._flags.contains || remaining.length === 0);
      }
      return this._check(passed, "expected members", expected);
    }
    respondTo(method) {
      const target = typeof this._actual === "function" && !this._flags.itself ? this._actual.prototype : this._actual;
      return this._check(target != null && typeof target[method] === "function", "expected to respond to " + method);
    }
    throw(errorLike, message) {
      if (typeof this._actual !== "function") { fail("expected a function to call — got " + fmt(this._actual)); }
      let thrown = false, error;
      try { this._actual(); } catch (caught) { thrown = true; error = caught; }
      let passed = thrown;
      if (thrown && errorLike !== undefined) {
        const text = error && error.message !== undefined ? String(error.message) : String(error);
        if (typeof errorLike === "function") { passed = error instanceof errorLike; }
        else if (errorLike instanceof RegExp) { passed = errorLike.test(text); }
        else { passed = text.includes(String(errorLike)); }
        if (passed && message !== undefined) {
          passed = message instanceof RegExp ? message.test(text) : text.includes(String(message));
        }
      }
      return this._check(passed, "expected the function to throw" + (errorLike === undefined ? "" : " " + fmt(errorLike instanceof RegExp ? String(errorLike) : errorLike && errorLike.name || errorLike)));
    }
    throws(errorLike, message) { return this.throw(errorLike, message); }
    Throw(errorLike, message) { return this.throw(errorLike, message); }
    jsonSchema(schema) {
      const problem = parse(bridge("schema.validate", { schema: schema, value: this._actual }));
      return this._check(!problem, "expected the value to match the JSON schema" + (problem ? " (" + problem + ")" : ""));
    }
  }
  function expect(value) { return new Assertion(value, {}); }

  // `pm.response.to…` (and a `pm.sendRequest` response's): Postman's response assertions, with
  // `not`, under the same strict lookup as `pm.expect`.
  class ResponseAssertion {
    constructor(raw, body, negate) {
      this._raw = raw;
      this._body = body;
      this._negate = !!negate;
      return strict(this, "response assertion");
    }
    get to() { return this; }
    get be() { return this; }
    get have() { return this; }
    get and() { return this; }
    get is() { return this; }
    get that() { return this; }
    get which() { return this; }
    get with() { return this; }
    get been() { return this; }
    get not() { return new ResponseAssertion(this._raw, this._body, !this._negate); }
    _check(passed, message) {
      if (this._negate ? passed : !passed) { fail(message + (this._negate ? " (negated)" : "")); }
      return this;
    }
    _status(test, label) {
      const code = this._raw.code;
      return this._check(test(code), "expected response status " + label + " — got " + code);
    }
    status(expected) {
      if (typeof expected === "number") { return this._status((code) => code === expected, String(expected)); }
      return this._check(String(this._raw.status) === String(expected), "expected response status " + expected + " — got " + this._raw.status);
    }
    get ok() { return this._status((code) => code === 200, "200"); }
    get success() { return this._status((code) => code >= 200 && code < 300, "2xx"); }
    get info() { return this._status((code) => code >= 100 && code < 200, "1xx"); }
    get redirection() { return this._status((code) => code >= 300 && code < 400, "3xx"); }
    get clientError() { return this._status((code) => code >= 400 && code < 500, "4xx"); }
    get serverError() { return this._status((code) => code >= 500 && code < 600, "5xx"); }
    get error() { return this._status((code) => code >= 400, "4xx or 5xx"); }
    get accepted() { return this._status((code) => code === 202, "202"); }
    get badRequest() { return this._status((code) => code === 400, "400"); }
    get unauthorized() { return this._status((code) => code === 401, "401"); }
    get forbidden() { return this._status((code) => code === 403, "403"); }
    get notFound() { return this._status((code) => code === 404, "404"); }
    get rateLimited() { return this._status((code) => code === 429, "429"); }
    header(name, value) {
      const header = (this._raw.headers || []).find((item) => item.key.toLowerCase() === String(name).toLowerCase());
      if (value === undefined) { return this._check(!!header, "expected response header " + name); }
      return this._check(!!header && header.value === String(value),
        "expected response header " + name + " to equal " + value + " — got " + (header ? header.value : "no header"));
    }
    body(expected) {
      const text = this._body();
      if (expected === undefined) { return this._check(text.length > 0, "expected a response body"); }
      if (expected instanceof RegExp) { return this._check(expected.test(text), "expected the response body to match " + String(expected)); }
      if (typeof expected === "string") { return this._check(text === expected, "expected the response body to equal " + fmt(expected)); }
      let parsed, valid = true;
      try { parsed = jsonParse(text); } catch (error) { valid = false; }
      return this._check(valid && deepEqual(parsed, expected), "expected the response JSON to deeply equal " + fmt(expected));
    }
    get withBody() { return this._check(this._body().length > 0, "expected a response body"); }
    get json() {
      let valid = true;
      try { jsonParse(this._body()); } catch (error) { valid = false; }
      return this._check(valid, "expected a JSON response body");
    }
    jsonBody(path, expected) {
      let value, valid = true;
      try { value = jsonParse(this._body()); } catch (error) { valid = false; }
      if (!valid) { return this._check(false, "expected a JSON response body"); }
      if (path !== undefined) {
        const found = nested(value, path);
        if (arguments.length === 1) { return this._check(found.has, "expected JSON body property " + path); }
        return this._check(found.has && deepEqual(found.value, expected),
          "expected JSON body property " + path + " to equal " + fmt(expected) + " — got " + fmt(found.value));
      }
      return this;
    }
    jsonSchema(schema) {
      let value, valid = true;
      try { value = jsonParse(this._body()); } catch (error) { valid = false; }
      if (!valid) { return this._check(false, "expected a JSON response body to validate against the schema"); }
      const problem = parse(bridge("schema.validate", { schema: schema, value: value }));
      return this._check(!problem, "expected the response to match the JSON schema" + (problem ? " — " + problem : ""));
    }
    responseTime(limit) { return this._check(this._raw.elapsed_ms <= limit, "expected a response time of at most " + limit + " ms — got " + this._raw.elapsed_ms); }
  }

  // A cookie list (`pm.cookies`, a response's `cookies`): Postman's PropertyList reads.
  function cookieList(list) {
    const find = (name) => list.find((cookie) => cookie.name === String(name));
    return {
      get(name) { const cookie = find(name); return cookie ? cookie.value : undefined; },
      has(name) { return find(name) !== undefined; },
      one(name) { const cookie = find(name); return cookie ? Object.assign({}, cookie) : undefined; },
      all() { return list.map((cookie) => Object.assign({}, cookie)); },
      count() { return list.length; },
      toObject() {
        const object = {};
        list.forEach((cookie) => { object[cookie.name] = cookie.value; });
        return object;
      }
    };
  }
  // `pm.cookies.jar()`: reads and writes of the workspace jar for the request's host and its
  // parent and sub domains. Each call answers its optional `(error, value)` callback at once
  // and also returns the value; without a callback an error throws.
  function cookieJar() {
    function call(op, payload, callback, pick) {
      let value;
      try {
        value = pick(parse(bridge(op, payload)));
      } catch (error) {
        if (typeof callback !== "function") { throw error; }
        callback(error, undefined);
        return undefined;
      }
      if (typeof callback === "function") { callback(null, value); }
      return value;
    }
    return {
      get(url, name, callback) {
        return call("cookies.get", { url: String(url), name: String(name) }, callback, (result) => result.value === null ? undefined : result.value);
      },
      getAll(url, callback) {
        return call("cookies.list", { url: String(url) }, callback, (result) => result.cookies);
      },
      set(url, name, value, callback) {
        let cookie;
        if (name !== null && typeof name === "object") {
          cookie = Object.assign({}, name);
          callback = value;
        } else {
          cookie = { name: String(name), value: value };
        }
        if (cookie.value !== undefined && cookie.value !== null && typeof cookie.value !== "string") { cookie.value = fmt(cookie.value); }
        return call("cookies.set", { url: String(url), cookie: cookie }, callback, () => Object.assign({}, cookie));
      },
      unset(url, name, callback) {
        return call("cookies.remove", { url: String(url), name: String(name) }, callback, () => undefined);
      },
      clear(url, callback) {
        return call("cookies.remove", { url: String(url) }, callback, () => undefined);
      }
    };
  }

  function headerObject(headers) {
    return {
      get(name) {
        const header = (headers || []).find((item) => item.key.toLowerCase() === String(name).toLowerCase());
        return header ? header.value : undefined;
      },
      has(name) { return this.get(name) !== undefined; },
      toObject() {
        const object = {};
        (headers || []).forEach((item) => { object[item.key] = item.value; });
        return object;
      }
    };
  }

  // Read-only view of plain data: every nested object and array is frozen.
  function deepFreeze(value) {
    if (value !== null && typeof value === "object") {
      Object.keys(value).forEach((key) => deepFreeze(value[key]));
      Object.freeze(value);
    }
    return value;
  }

  // `loadBody` fetches the body on first use, so a script that never reads it never copies it.
  function responseJson(raw, loadBody) {
    let cachedBody;
    function body() {
      if (cachedBody === undefined) { cachedBody = loadBody(); }
      return cachedBody;
    }
    const result = {
      code: raw.code,
      status: raw.status,
      responseTime: raw.elapsed_ms,
      responseSize: raw.size,
      headers: Object.assign(headerObject(raw.headers), {
        get(name) {
          const header = (raw.headers || []).find((item) => item.key.toLowerCase() === String(name).toLowerCase());
          return header ? header.value : undefined;
        }
      }),
      cookies: cookieList(raw.cookies || []),
      text() { return body(); },
      json() { return jsonParse(body()); },
      to: new ResponseAssertion(raw, body, false)
    };
    // `pm.response.ai` exists only for AI API requests.
    if (raw.ai !== undefined) {
      Object.defineProperty(result, "ai", { value: deepFreeze(raw.ai), enumerable: true });
    }
    return result;
  }

  const pending = [];
  // Called by the host after the promise jobs ran: an async test that never settled fails.
  Object.defineProperty(globalThis, "__pdSettle", {
    value() {
      pending.forEach((entry) => {
        if (!entry.done) {
          entry.done = true;
          bridge("test.record", { name: entry.name, passed: false, skipped: false, error: "Async pm.test did not finish" });
        }
      });
    }
  });

  // Byte strings and the CryptoJS subset (MD5, SHA1, SHA256, SHA512; HmacSHA1, HmacSHA256,
  // HmacSHA512; Hex, Base64, Utf8) are
  // computed by the host; a WordArray here is only its lowercase hex.
  function WordArray(hex) { this._hex = hex; }
  WordArray.prototype.toString = function (encoder) { return (encoder || Hex).stringify(this); };
  function bytesOf(value) {
    return value instanceof WordArray ? { hex: value._hex } : { text: String(value) };
  }
  const Hex = {
    stringify(words) { return words._hex; },
    parse(text) { return new WordArray(bridge("bytes.convert", { from: "hex", to: "hex", value: String(text) })); }
  };
  const Base64 = {
    stringify(words) { return bridge("bytes.convert", { from: "hex", to: "base64", value: words._hex }); },
    parse(text) { return new WordArray(bridge("bytes.convert", { from: "base64", to: "hex", value: String(text) })); }
  };
  const Utf8 = {
    stringify(words) { return bridge("bytes.convert", { from: "hex", to: "utf8", value: words._hex }); },
    parse(text) { return new WordArray(bridge("bytes.convert", { from: "utf8", to: "hex", value: String(text) })); }
  };
  function digest(algorithm) {
    return (message) => new WordArray(bridge("crypto.digest", { algorithm: algorithm, data: bytesOf(message) }));
  }
  function hmac(algorithm) {
    return (message, key) => new WordArray(bridge("crypto.hmac", { algorithm: algorithm, data: bytesOf(message), key: bytesOf(key) }));
  }
  const CryptoJS = {
    MD5: digest("md5"),
    SHA1: digest("sha1"),
    SHA256: digest("sha256"),
    SHA512: digest("sha512"),
    HmacSHA1: hmac("sha1"),
    HmacSHA256: hmac("sha256"),
    HmacSHA512: hmac("sha512"),
    enc: { Hex: Hex, Base64: Base64, Utf8: Utf8 }
  };
  globalThis.CryptoJS = CryptoJS;
  // The `tv4.validate(data, schema)` subset Postman scripts use, on the JSON Schema validator
  // of the `JsonSchemaValid` assertion; `tv4.error` holds the last violation or null.
  const tv4 = {
    error: null,
    validate(data, schema) {
      const problem = parse(bridge("schema.validate", { schema: schema, value: data }));
      tv4.error = problem ? { message: problem } : null;
      return !problem;
    }
  };
  globalThis.tv4 = tv4;
  globalThis.btoa = function (text) { return bridge("bytes.convert", { from: "latin1", to: "base64", value: String(text) }); };
  globalThis.atob = function (text) { return bridge("bytes.convert", { from: "base64", to: "latin1", value: String(text) }); };

  function prepare() {
  const info = parse(bridge("info"));
  const requestRaw = parse(bridge("request.get"));
  const responseRaw = parse(bridge("response.get"));
  const response = responseRaw.available ? responseJson(responseRaw, () => bridge("response.body")) : undefined;

  const request = {
    get method() { return requestRaw.method; },
    set method(value) { bridge("request.method.set", { method: value }); requestRaw.method = String(value).toUpperCase(); },
    get url() {
      const self = this;
      const urlObject = {
        toString() { return parse(bridge("request.url.get")).url; },
        addQueryParams(params) {
          const normalized = Array.isArray(params) ? params : Object.keys(params || {}).map((key) => ({ key: key, value: params[key] }));
          bridge("request.query.add", { params: normalized });
        },
        getQueryString() {
          const url = self.url.toString();
          const index = url.indexOf("?");
          return index === -1 ? "" : url.slice(index + 1);
        }
      };
      urlObject.query = {
        add(params) { urlObject.addQueryParams(params); },
        get(name) {
          const pairs = urlObject.getQueryString().split("&");
          for (const pair of pairs) {
            const index = pair.indexOf("=");
            const key = decodeURIComponent(index === -1 ? pair : pair.slice(0, index));
            if (key === String(name)) {
              return index === -1 ? "" : decodeURIComponent(pair.slice(index + 1));
            }
          }
          return undefined;
        },
        has(name) { return urlObject.query.get(name) !== undefined; },
        remove(name) { bridge("request.query.remove", { key: String(name) }); }
      };
      return urlObject;
    },
    headers: {
      get(name) {
        const result = parse(bridge("request.header.get", { name: name }));
        return result.value === null ? undefined : result.value;
      },
      has(name) { return this.get(name) !== undefined; },
      add(header) { bridge("request.header.add", header); },
      upsert(header) { bridge("request.header.upsert", header); },
      remove(name) { bridge("request.header.remove", { name: name }); },
      toObject() { return parse(bridge("request.headers.list")); }
    },
    body: {
      get raw() { return parse(bridge("request.body.get")).raw; },
      update(content) { bridge("request.body.update", { content: content }); }
    }
  };

  function makeScope(name) {
    return {
      get(key) {
        const result = parse(bridge("var.get", { scope: name, key: String(key) }));
        return result.found ? result.value : undefined;
      },
      has(key) {
        return parse(bridge("var.get", { scope: name, key: String(key) })).found;
      },
      set(key, value) { bridge("var.set", { scope: name, key: String(key), value: fmt(value) }); },
      unset(key) { bridge("var.unset", { scope: name, key: String(key) }); },
      clear() { bridge("var.clear", { scope: name }); },
      toObject() { return parse(bridge("var.list", { scope: name })); },
      replaceIn(text) { return bridge("replaceIn", { text: String(text) }); }
    };
  }

  // `pm.cookies` is read on first use: the jar cookies of the request URL (and, after the
  // response, its own cookies).
  let cookies;
  const pm = {
    info: info,
    request: request,
    response: response,
    environment: makeScope("environment"),
    collectionVariables: makeScope("collection"),
    globals: makeScope("globals"),
    variables: makeScope("variables"),
    iterationData: makeScope("data"),
    execution: {
      setNextRequest(name) { bridge("next", { name: name === null ? null : String(name) }); }
    },
    test(name, fn) {
      try {
        const result = fn();
        if (result != null && (typeof result === "object" || typeof result === "function") && typeof result.then === "function") {
          // Recorded when it settles; the host runs promise jobs after the script.
          const entry = { name: String(name), done: false };
          pending.push(entry);
          const settle = (passed, error) => {
            if (entry.done) { return; }
            entry.done = true;
            bridge("test.record", passed ? { name: entry.name, passed: true, skipped: false }
              : { name: entry.name, passed: false, skipped: false, error: String(error && error.message || error) });
          };
          Promise.resolve(result).then(() => settle(true), (error) => settle(false, error));
        } else {
          bridge("test.record", { name: String(name), passed: true, skipped: false });
        }
      } catch (error) {
        bridge("test.record", { name: String(name), passed: false, skipped: false, error: String(error && error.message || error) });
      }
      return pm;
    },
    expect: expect,
    sendRequest(requestOrUrl, callback) {
      let res;
      try {
        const requestObject = typeof requestOrUrl === "string" ? { url: requestOrUrl } : requestOrUrl;
        const headers = [];
        const headerSource = requestObject.header !== undefined ? requestObject.header : requestObject.headers;
        if (Array.isArray(headerSource)) {
          headerSource.forEach((item) => {
            if (item && !item.disabled) { headers.push({ key: String(item.key), value: String(item.value) }); }
          });
        } else if (headerSource && typeof headerSource === "object") {
          Object.keys(headerSource).forEach((key) => headers.push({ key: key, value: String(headerSource[key]) }));
        }
        const url = requestObject.url !== null && typeof requestObject.url === "object"
          ? (requestObject.url.raw !== undefined ? requestObject.url.raw : String(requestObject.url))
          : requestObject.url;
        // The body goes to the host as written (text or `{ mode, … }`); it is encoded by its mode
        // there, so `urlencoded`, `formdata` and `graphql` bodies are sent the way Postman sends them.
        const raw = parse(bridge("http.send", {
          url: String(url),
          method: String(requestObject.method || "GET"),
          headers: headers,
          body: requestObject.body === undefined ? null : requestObject.body
        }));
        res = responseJson(raw, () => raw.body);
      } catch (error) {
        if (typeof callback !== "function") { throw error; }
        callback(error, undefined);
        return;
      }
      // Outside the try so a throwing callback runs once and its error propagates.
      if (typeof callback === "function") { callback(null, res); }
    },
    get cookies() {
      if (cookies === undefined) {
        cookies = Object.assign(cookieList(parse(bridge("cookies.request"))), { jar: cookieJar });
      }
      return cookies;
    }
  };
  pm.test.skip = function (name) {
    bridge("test.record", { name: String(name), passed: true, skipped: true });
    return pm;
  };

  function format(args) {
    let out = "";
    for (let i = 0; i < args.length; i++) {
      const text = fmt(args[i]);
      out += (i === 0 ? "" : " ") + (text === undefined ? "" : text);
    }
    return out;
  }
  const console = {
    log() { bridge("log", { level: "log", message: format(arguments) }); },
    info() { bridge("log", { level: "info", message: format(arguments) }); },
    warn() { bridge("log", { level: "warn", message: format(arguments) }); },
    error() { bridge("log", { level: "error", message: format(arguments) }); },
    debug() { bridge("log", { level: "debug", message: format(arguments) }); }
  };
  globalThis.console = console;
  globalThis.pm = pm;
  // The legacy `postman.*` variable and flow calls, on the `pm` scopes they stand for.
  globalThis.postman = {
    setEnvironmentVariable(key, value) { pm.environment.set(key, value); },
    getEnvironmentVariable(key) { return pm.environment.get(key); },
    clearEnvironmentVariable(key) { pm.environment.unset(key); },
    setGlobalVariable(key, value) { pm.globals.set(key, value); },
    getGlobalVariable(key) { return pm.globals.get(key); },
    clearGlobalVariable(key) { pm.globals.unset(key); },
    setNextRequest(name) { pm.execution.setNextRequest(name); }
  };
  }

  prepare();
})()
"#;

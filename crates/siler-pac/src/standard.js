// The standard functions of a proxy script (Netscape's PAC format). `__resolve`, which
// looks a name up, and `__myAddress` come from the helper.
(function (global) {
  "use strict";
  const resolve = global.__resolve;
  const myAddress = global.__myAddress;
  delete global.__resolve;
  delete global.__myAddress;

  const DAYS = ["SUN", "MON", "TUE", "WED", "THU", "FRI", "SAT"];
  const MONTHS = ["JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC"];
  const IPV4 = /^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/;

  function number(address) {
    const m = IPV4.exec(String(address));
    if (!m) return null;
    let out = 0;
    for (let i = 1; i <= 4; i++) {
      if (Number(m[i]) > 255) return null;
      out = out * 256 + Number(m[i]);
    }
    return out;
  }

  function dnsResolve(host) {
    const name = String(host);
    if (number(name) !== null) return name;
    return resolve(name) || null;
  }

  function within(value, from, to) {
    // A range that ends before it starts runs over the turn of the year, month, week or day.
    return from <= to ? from <= value && value <= to : value >= from || value <= to;
  }

  function clock(args) {
    const gmt = args[args.length - 1] === "GMT";
    const now = new Date();
    return {
      args: gmt ? args.slice(0, -1) : args,
      day: gmt ? now.getUTCDay() : now.getDay(),
      date: gmt ? now.getUTCDate() : now.getDate(),
      month: gmt ? now.getUTCMonth() : now.getMonth(),
      year: gmt ? now.getUTCFullYear() : now.getFullYear(),
      hours: gmt ? now.getUTCHours() : now.getHours(),
      minutes: gmt ? now.getUTCMinutes() : now.getMinutes(),
      seconds: gmt ? now.getUTCSeconds() : now.getSeconds(),
    };
  }

  function weekdayRange(...given) {
    const now = clock(given);
    const from = DAYS.indexOf(now.args[0]);
    const to = now.args.length > 1 ? DAYS.indexOf(now.args[1]) : from;
    return from >= 0 && to >= 0 && within(now.day, from, to);
  }

  function dateRange(...given) {
    const now = clock(given);
    const parts = now.args.map((value) => {
      if (typeof value === "string" && MONTHS.includes(value)) {
        return { unit: "month", value: MONTHS.indexOf(value) };
      }
      const n = Number(value);
      return { unit: n >= 1 && n <= 31 ? "date" : n > 31 ? "year" : "", value: n };
    });
    if (parts.length === 0 || parts.some((p) => !p.unit)) return false;
    const half = parts.length / 2;
    const range = parts.length % 2 === 0 && parts.slice(0, half).every((p, i) => p.unit === parts[half + i].unit);
    const from = range ? parts.slice(0, half) : parts;
    const to = range ? parts.slice(half) : parts;
    const size = { year: 10000 * 100, month: 100, date: 1 };
    const total = (list, pick) => list.reduce((sum, p) => sum + size[p.unit] * pick(p), 0);
    const given0 = total(from, (p) => p.value);
    const given1 = total(to, (p) => p.value);
    const today = total(from, (p) => now[p.unit]);
    if (given0 > given1 && from.some((p) => p.unit === "year")) return false;
    return within(today, given0, given1);
  }

  function timeRange(...given) {
    const now = clock(given);
    const a = now.args.map(Number);
    const at = (h, m, s) => h * 3600 + m * 60 + s;
    switch (a.length) {
      case 1:
        return now.hours === a[0];
      case 2:
        return within(now.hours, a[0], a[1]);
      case 4:
        return within(at(now.hours, now.minutes, 0), at(a[0], a[1], 0), at(a[2], a[3], 0));
      case 6:
        return within(at(now.hours, now.minutes, now.seconds), at(a[0], a[1], a[2]), at(a[3], a[4], a[5]));
    }
    return false;
  }

  Object.assign(global, {
    dnsResolve,
    weekdayRange,
    dateRange,
    timeRange,
    isResolvable: (host) => dnsResolve(host) !== null,
    myIpAddress: () => myAddress(),
    isPlainHostName: (host) => !/[.:]/.test(String(host)),
    dnsDomainLevels: (host) => String(host).split(".").length - 1,
    convert_addr: (address) => number(address) || 0,
    alert: () => {},
    dnsDomainIs(host, domain) {
      const h = String(host).toLowerCase();
      const d = String(domain).toLowerCase();
      if (d.startsWith(".")) return h.endsWith(d) || h === d.slice(1);
      return h === d || h.endsWith("." + d);
    },
    localHostOrDomainIs(host, full) {
      const h = String(host);
      return h === String(full) || (!h.includes(".") && String(full).startsWith(h + "."));
    },
    isInNet(host, network, mask) {
      const net = number(network);
      const bits = number(mask);
      if (net === null || bits === null) return false;
      const address = number(dnsResolve(host));
      return address !== null && (address & bits) >>> 0 === (net & bits) >>> 0;
    },
    shExpMatch(text, pattern) {
      // Matched by hand: a pattern turned into a regular expression can take for ever.
      const s = String(text);
      const p = String(pattern);
      let i = 0;
      let j = 0;
      let star = -1;
      let resume = 0;
      while (i < s.length) {
        if (j < p.length && p[j] === "*") {
          star = j++;
          resume = i;
        } else if (j < p.length && (p[j] === "?" || p[j] === s[i])) {
          i++;
          j++;
        } else if (star >= 0) {
          j = star + 1;
          i = ++resume;
        } else {
          return false;
        }
      }
      while (p[j] === "*") j++;
      return j === p.length;
    },
  });
})(globalThis);

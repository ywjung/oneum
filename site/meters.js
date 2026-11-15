// The two VU meters of the faceplate, drawn like the ones in the app and moved by a
// made-up signal (no sound is played, nothing is loaded). They stand still for
// visitors who asked their system for less motion, while the band is out of view,
// and when the button under the name says so.
(function () {
  "use strict";

  var SVG = "http://www.w3.org/2000/svg";
  // The scale of a VU meter: the needle's way is proportional to the signal's
  // amplitude, so the decibel marks crowd together at the left.
  var MARKS = [-20, -10, -7, -5, -3, -2, -1, 0, 1, 2, 3];
  var SWEEP = 46; // degrees to either side of straight up
  var PIVOT = { x: 160, y: 196 };
  var RADIUS = 128;

  function place(decibels) {
    var full = Math.pow(10, 3 / 20);
    return Math.min(Math.max(Math.pow(10, decibels / 20) / full, 0), 1);
  }

  function angle(fraction) {
    return -SWEEP + 2 * SWEEP * fraction;
  }

  function point(degrees, radius) {
    var radians = (degrees * Math.PI) / 180;
    return {
      x: PIVOT.x + radius * Math.sin(radians),
      y: PIVOT.y - radius * Math.cos(radians),
    };
  }

  function make(name, attributes, parent) {
    var element = document.createElementNS(SVG, name);
    for (var key in attributes) {
      element.setAttribute(key, attributes[key]);
    }
    if (parent) {
      parent.appendChild(element);
    }
    return element;
  }

  function arc(from, to, radius) {
    var a = point(from, radius);
    var b = point(to, radius);
    return "M" + a.x.toFixed(1) + " " + a.y.toFixed(1) + " A" + radius + " " + radius +
      " 0 0 1 " + b.x.toFixed(1) + " " + b.y.toFixed(1);
  }

  function draw(holder, channel) {
    var svg = make("svg", { viewBox: "0 0 320 200", role: "img" });
    make("title", {}, svg).textContent = "VU " + channel;
    make("rect", { x: 2, y: 2, width: 316, height: 196, rx: 14, fill: "#15151c" }, svg);
    make("rect", { x: 12, y: 12, width: 296, height: 176, rx: 6, fill: "#f2e8c6" }, svg);
    var clip = make("clipPath", { id: "face-" + channel }, svg);
    make("rect", { x: 12, y: 12, width: 296, height: 176, rx: 6 }, clip);
    var face = make("g", { "clip-path": "url(#face-" + channel + ")" }, svg);

    // The scale: black up to 0, red above it.
    make("path", {
      d: arc(angle(place(-20)), angle(place(0)), RADIUS),
      fill: "none", stroke: "#1b1b22", "stroke-width": 2.2,
    }, face);
    make("path", {
      d: arc(angle(place(0)), angle(1), RADIUS - 3),
      fill: "none", stroke: "#c9362b", "stroke-width": 8,
    }, face);
    MARKS.forEach(function (mark) {
      var at = angle(place(mark));
      var outer = point(at, RADIUS + 9);
      var inner = point(at, RADIUS);
      make("line", {
        x1: inner.x.toFixed(1), y1: inner.y.toFixed(1),
        x2: outer.x.toFixed(1), y2: outer.y.toFixed(1),
        stroke: mark > 0 ? "#c9362b" : "#1b1b22", "stroke-width": 2,
      }, face);
      var label = point(at, RADIUS + 21);
      var text = make("text", {
        x: label.x.toFixed(1), y: (label.y + 4).toFixed(1), "text-anchor": "middle",
        "font-size": 12, "font-family": "Helvetica, Arial, sans-serif",
        fill: mark > 0 ? "#c9362b" : "#1b1b22",
      }, face);
      text.textContent = Math.abs(mark);
    });
    var vu = make("text", {
      x: 160, y: 138, "text-anchor": "middle", "font-size": 19,
      "font-family": "Helvetica, Arial, sans-serif", fill: "#1b1b22",
    }, face);
    vu.textContent = "VU";
    var side = make("text", {
      x: channel === "L" ? 26 : 294, y: 178, "text-anchor": "middle", "font-size": 14,
      "font-family": "Helvetica, Arial, sans-serif", fill: "#1b1b22",
    }, face);
    side.textContent = channel;
    var peak = make("text", {
      x: 268, y: 31, "text-anchor": "end", "font-size": 8.5,
      "font-family": "Helvetica, Arial, sans-serif", fill: "#5b5642",
    }, face);
    peak.textContent = "PEAK";
    var lamp = make("circle", { cx: 280, cy: 28, r: 5, fill: "#5a2a25" }, face);

    var needle = make("line", {
      x1: PIVOT.x, y1: PIVOT.y, x2: PIVOT.x, y2: PIVOT.y - RADIUS - 6,
      stroke: "#1b1b22", "stroke-width": 2.4, "stroke-linecap": "round",
    }, face);
    make("circle", { cx: PIVOT.x, cy: PIVOT.y + 6, r: 34, fill: "#15151c" }, face);
    holder.appendChild(svg);
    return { needle: needle, lamp: lamp, at: 0, lit: 0 };
  }

  function turn(meter) {
    meter.needle.setAttribute(
      "transform",
      "rotate(" + angle(meter.at).toFixed(2) + " " + PIVOT.x + " " + PIVOT.y + ")"
    );
    meter.lamp.setAttribute("fill", meter.lit > 0 ? "#ff5a3c" : "#5a2a25");
  }

  // A signal that swells and falls like music does: slow waves, a beat, and now and
  // then a louder passage. Left and right share most of it.
  function level(time, channel) {
    var shift = channel === "L" ? 0 : 1.7;
    var slow = 0.5 + 0.5 * Math.sin(time * 0.31 + shift * 0.4);
    var phrase = 0.5 + 0.5 * Math.sin(time * 0.83 + 1.1 + shift);
    var beat = Math.pow(0.5 + 0.5 * Math.sin(time * 6.4 + shift * 0.2), 3);
    var swell = Math.max(0, Math.sin(time * 0.11 + 0.6)) * 0.35;
    var decibels = -16 + 9 * slow + 5 * phrase + 3 * beat + 6 * swell;
    return place(decibels);
  }

  function start() {
    var holders = document.querySelectorAll(".vu[data-channel]");
    if (!holders.length) {
      return;
    }
    var meters = [];
    holders.forEach(function (holder) {
      var channel = holder.getAttribute("data-channel");
      var meter = draw(holder, channel);
      meter.channel = channel;
      meters.push(meter);
    });
    var rest = place(-7);
    var still = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    var inView = true;
    var button = document.querySelector("[data-meters]");
    var last = null;
    var clock = 0;

    function settle() {
      meters.forEach(function (meter) {
        meter.at = rest;
        meter.lit = 0;
        turn(meter);
      });
    }

    function frame(now) {
      if (still || !inView || document.hidden) {
        last = null;
        return;
      }
      var step = last === null ? 0 : Math.min((now - last) / 1000, 0.1);
      last = now;
      clock += step;
      meters.forEach(function (meter) {
        var target = level(clock, meter.channel);
        // A VU needle reaches a steady tone in about a third of a second.
        meter.at += (target - meter.at) * (1 - Math.exp(-step / 0.075));
        meter.lit = target > place(1.5) ? 0.35 : Math.max(0, meter.lit - step);
        turn(meter);
      });
      window.requestAnimationFrame(frame);
    }

    function run() {
      if (!still && inView && !document.hidden && last === null) {
        window.requestAnimationFrame(frame);
      }
    }

    function label() {
      if (button) {
        button.textContent = still
          ? button.getAttribute("data-move")
          : button.getAttribute("data-stop");
        button.setAttribute("aria-pressed", still ? "false" : "true");
      }
    }

    settle();
    label();
    if (button) {
      button.hidden = false;
      button.addEventListener("click", function () {
        still = !still;
        label();
        if (still) {
          settle();
        } else {
          run();
        }
      });
    }
    if ("IntersectionObserver" in window) {
      new IntersectionObserver(function (entries) {
        inView = entries[0].isIntersecting;
        run();
      }).observe(holders[0].closest(".faceplate") || holders[0]);
    }
    document.addEventListener("visibilitychange", run);
    run();
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", start);
  } else {
    start();
  }
})();

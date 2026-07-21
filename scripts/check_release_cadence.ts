interface PublishedRelease {
  tagName: string;
  publishedAt: string;
}

export function sameDayRelease(
  releases: readonly PublishedRelease[],
  releaseTag: string,
  now: Date,
  timeZone: string,
): PublishedRelease | undefined {
  if (releases.some((release) => release.tagName === releaseTag)) {
    return undefined;
  }
  const currentDay = localDay(now, timeZone);
  return releases.find((release) => {
    const publishedAt = new Date(release.publishedAt);
    return !Number.isNaN(publishedAt.valueOf()) &&
      localDay(publishedAt, timeZone) === currentDay;
  });
}

function localDay(date: Date, timeZone: string): string {
  const parts = new Intl.DateTimeFormat("en-US", {
    timeZone,
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
  }).formatToParts(date);
  const value = (type: Intl.DateTimeFormatPartTypes) =>
    parts.find((part) => part.type === type)?.value;
  return `${value("year")}-${value("month")}-${value("day")}`;
}

export function parseArguments(arguments_: string[]) {
  let releaseTag = "";
  let releasesPath = "";
  let timeZone = "Asia/Shanghai";
  let now = new Date();

  for (let index = 0; index < arguments_.length; index += 1) {
    const argument = arguments_[index];
    const value = () => {
      const candidate = arguments_[index + 1];
      if (!candidate) throw new Error(`${argument} requires a value`);
      index += 1;
      return candidate;
    };
    switch (argument) {
      case "--tag":
        releaseTag = value();
        break;
      case "--releases":
        releasesPath = value();
        break;
      case "--time-zone":
        timeZone = value();
        break;
      case "--now": {
        now = new Date(value());
        if (Number.isNaN(now.valueOf())) throw new Error("--now is invalid");
        break;
      }
      default:
        throw new Error(`unknown release cadence argument: ${argument}`);
    }
  }
  if (!releaseTag || !releasesPath) {
    throw new Error("--tag and --releases are required");
  }
  return { releaseTag, releasesPath, timeZone, now };
}

function decodeReleases(value: unknown): PublishedRelease[] {
  if (!Array.isArray(value)) {
    throw new Error("release inventory must be an array");
  }
  return value.map((release) => {
    if (
      !release || typeof release !== "object" ||
      typeof release.tagName !== "string" ||
      typeof release.publishedAt !== "string"
    ) {
      throw new Error("release inventory contains an invalid record");
    }
    return {
      tagName: release.tagName,
      publishedAt: release.publishedAt,
    };
  });
}

if (import.meta.main) {
  const options = parseArguments(Deno.args);
  const releases = decodeReleases(
    JSON.parse(await Deno.readTextFile(options.releasesPath)),
  );
  const existing = sameDayRelease(
    releases,
    options.releaseTag,
    options.now,
    options.timeZone,
  );
  if (existing) {
    throw new Error(
      `release cadence allows at most one release per ${options.timeZone} day; ` +
        `${existing.tagName} was already published today.`,
    );
  }
  console.log(`release cadence accepted for ${options.releaseTag}`);
}

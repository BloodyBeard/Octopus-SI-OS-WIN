const { runFreshOnboarding } = require("./onboarding-support.cjs");

runFreshOnboarding(process.argv.slice(2)).catch((error) => {
  console.error(error.safeMessage || "Fresh onboarding could not start. See the private report.");
  process.exitCode = error.exitCode || 10;
});

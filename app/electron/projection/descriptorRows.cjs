module.exports = { descriptorRowsProjection };

const fs = require("node:fs/promises");
const path = require("node:path");
const { readJsonFile, readOptionalRegularJsonDocument } = require("./jsonFiles.cjs");
const { readApplicationLogoAsset, readInterfacePrivateConfigurationProjection } = require("./privateConfiguration.cjs");
const { normalizeDescriptorRow } = require("./registryRows.cjs");
const { parseApplicationManifest, parseClientWorkerTemplate, parseOctopusOsSidecar } = require("./sidecars.cjs");
const { mapConcurrentOrdered, stringValue } = require("./values.cjs");

function descriptorRowsProjection(store) {
  async function readDescriptorRows(registryRoot, registry, warnings = [], launchEnvironment = null, names = null) {
    const root = path.join(registryRoot, registry.kind);
    let entries;
    try {
      entries = await fs.readdir(root, { withFileTypes: true });
    } catch (error) {
      if (error?.code === "ENOENT") return [];
      throw error;
    }
    const transferReadiness = new Map();
    const rows = await mapConcurrentOrdered(
      entries.filter((entry) => entry.isDirectory() && (!names || names.has(entry.name))),
      32,
      async (entry) => {
        const descriptorPath = path.join(root, entry.name, registry.file);
        let descriptor;
        try {
          descriptor = JSON.parse(await fs.readFile(descriptorPath, "utf8"));
        } catch (error) {
          if (error?.code === "ENOENT") return null;
          warnings.push(`descriptor parse failed: ${descriptorPath}: ${error.message}`);
          return null;
        }
        let row = normalizeDescriptorRow(registryRoot, registry, entry.name, descriptorPath, descriptor);
        if (registry.kind === "interface") {
          const octopusOsPath = path.join(root, entry.name, "octopus_os.json");
          try {
            const octopusOs = await readOptionalRegularJsonDocument(
              octopusOsPath,
              `Octopus OS sidecar ${entry.name}`,
            );
            if (octopusOs) {
              row.octopus_os_sidecar = {
                ...parseOctopusOsSidecar(octopusOs.document, entry.name, octopusOsPath),
                source_digest: octopusOs.digest,
              };
            }
          } catch (error) {
            row.octopus_os_sidecar = null;
            warnings.push(String(error?.message ?? error));
          }
          const applicationPath = path.join(root, entry.name, "application.json");
          try {
            const application = await readOptionalRegularJsonDocument(
              applicationPath,
              `application manifest ${entry.name}`,
            );
            if (application) {
              const manifest = parseApplicationManifest(
                application.document,
                entry.name,
                applicationPath,
              );
              row.application_manifest = {
                ...manifest,
                logo_data_url: await readApplicationLogoAsset(
                  path.join(root, entry.name),
                  manifest.logo,
                  `application logo ${entry.name}`,
                ),
                logo_dark_data_url: manifest.logo_dark
                  ? await readApplicationLogoAsset(
                      path.join(root, entry.name),
                      manifest.logo_dark,
                      `application dark logo ${entry.name}`,
                    )
                  : "",
              };
              row.application_manifest_path = applicationPath;
              row.application_manifest_digest = application.digest;
            }
          } catch (error) {
            row.application_manifest = null;
            warnings.push(String(error?.message ?? error));
          }
          try {
            row.interface_private_configuration = await readInterfacePrivateConfigurationProjection({
              descriptor,
              interfaceId: entry.name,
              interfaceRegistryDirectory: path.join(root, entry.name),
              interfacePrivateDirectory: path.join(store.octopusHome(), "interfaces", entry.name),
              launchEnvironment,
            });
          } catch (error) {
            const message = String(error?.message ?? error);
            row.interface_private_configuration = {
              interface_id: entry.name,
              status: "invalid",
              document_status: "invalid",
              document_digest: "",
              fields: [],
              error: message,
            };
            warnings.push(message);
          }
          const activationPath = path.join(root, entry.name, "activation.json");
          const activation = await readJsonFile(activationPath);
          row.activation_sidecars = activation ? [{
            ...activation,
            descriptor_path: activationPath,
            outbound_interface: stringValue(descriptor.outbound?.interface),
          }] : [];
          if (
            stringValue(descriptor.interface_type).toLowerCase() === "system"
            && stringValue(descriptor.role).toLowerCase() === "worker"
          ) {
            const workerPolicyPath = path.join(root, entry.name, "policy.json");
            row.worker_policy_path = workerPolicyPath;
            try {
              const workerPolicy = await readJsonFile(workerPolicyPath);
              row.worker_policy = workerPolicy;
              if (!workerPolicy) {
                row.worker_policy_status = "missing";
              } else if (
                !workerPolicy
                || typeof workerPolicy !== "object"
                || Array.isArray(workerPolicy)
                || stringValue(workerPolicy.id) !== stringValue(row.rule_id)
                || !workerPolicy.ceiling
                || typeof workerPolicy.ceiling !== "object"
                || Array.isArray(workerPolicy.ceiling)
              ) {
                row.worker_policy_status = "invalid";
                warnings.push(`worker policy is malformed or does not match its interface: ${workerPolicyPath}`);
              } else {
                row.worker_policy_status = "registered";
              }
            } catch (error) {
              row.worker_policy = null;
              row.worker_policy_status = "invalid";
              warnings.push(String(error?.message ?? error));
            }
            const clientTemplatePath = path.join(root, entry.name, "client_template.json");
            row.client_worker_template_path = clientTemplatePath;
            try {
              const clientTemplate = await readOptionalRegularJsonDocument(
                clientTemplatePath,
                `client worker template ${entry.name}`,
              );
              row.client_worker_template = clientTemplate
                ? parseClientWorkerTemplate(clientTemplate.document, entry.name, clientTemplatePath)
                : null;
              row.client_worker_template_digest = clientTemplate?.digest ?? "";
              row.client_worker_template_status = clientTemplate ? "registered" : "absent";
            } catch (error) {
              row.client_worker_template = null;
              row.client_worker_template_digest = "";
              row.client_worker_template_status = "invalid";
              warnings.push(String(error?.message ?? error));
            }
          }
          row = await store.attachInterfaceArtifactTransferProjection(row, transferReadiness);
        }
        return row;
      },
    );
    return rows
      .filter(Boolean)
      .sort((a, b) => stringValue(a.name ?? a.id).localeCompare(stringValue(b.name ?? b.id)));
  }

  return { readDescriptorRows };
}

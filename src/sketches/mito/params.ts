import { ALL_ENVIRONMENTS } from "./game/environment";

const PARAMS_DEFAULT = {
    isRealtime: true,
    environment: "Temperate" as keyof typeof ALL_ENVIRONMENTS,
    cellEnergyMax: 20000,
    tissueInventoryCapacity: 10,
    rootTurnsPerTransfer: 20,
    leafReactionRate: 0.015,
    leafSugarPerReaction: 1,
    cellGestationTurns: 0,
    cellDiffusionWater: 0.0,
    cellDiffusionSugar: 0.0,
    soilDarknessBase: 0.2,
    soilDiffusionType: "discrete",
    soilDiffusionWater: 0.001,
    veinDiffusion: 0.5,
    soilMaxWater: 20,
    droop: 0.03,
    fountainTurnsPerWater: 11,
    fountainAppearanceRate: 1.5,
    transportTurnsPerMove: 5,
    sunlightReintroduction: 0.15,
    sunlightDiffusion: 0.0,
    maxResources: 100,
};

type Params = typeof PARAMS_DEFAULT;

export const params = { ...PARAMS_DEFAULT };

if (location.hash.length > 0) {
    const urlHashParams = JSON.parse(decodeURI(location.hash.substr(1))) as Partial<Params>;
    Object.assign(params, urlHashParams);
}

export function updateParamsHash() {
    const nonDefaultParams = Object.fromEntries(
        Object.entries(params).filter(
            ([key, value]) => value !== PARAMS_DEFAULT[key as keyof Params]
        )
    ) as Partial<Params>;

    location.hash = Object.keys(nonDefaultParams).length > 0
        ? encodeURI(JSON.stringify(nonDefaultParams))
        : "";
}
updateParamsHash();

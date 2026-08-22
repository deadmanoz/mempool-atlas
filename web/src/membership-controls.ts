const MEMBERSHIP_CONTROL_IDS = [
  "fee-age-tab",
  "classification-lens-select",
  "minimum-fee-rate",
  "maximum-age",
  "minimum-vsize",
  "reset-filters",
] as const;

const membershipControl = (
  id: (typeof MEMBERSHIP_CONTROL_IDS)[number],
): HTMLButtonElement | HTMLInputElement | HTMLSelectElement => {
  const control = document.getElementById(id);
  if (!(
    control instanceof HTMLButtonElement ||
    control instanceof HTMLInputElement ||
    control instanceof HTMLSelectElement
  )) {
    throw new Error(`Missing membership control #${id}`);
  }
  return control;
};

export const setMembershipControlsComplete = (complete: boolean): void => {
  for (const id of MEMBERSHIP_CONTROL_IDS) {
    membershipControl(id).disabled = !complete;
  }
};

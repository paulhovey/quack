// The page's plates, in order: their anchors, thumb-tab labels and tab colours.
export const plates = [
	{ id: 'top', tab: 'quack', color: 'var(--color-duck)' },
	{ id: 'grinder', tab: 'Grinder', color: 'var(--color-beak)' },
	{ id: 'session', tab: 'In the wild', color: 'var(--color-sky)' },
	{ id: 'field-marks', tab: 'Field marks', color: 'var(--color-leaf)' },
	{ id: 'measured', tab: 'Measured', color: 'var(--color-pink)' },
	{ id: 'organisations', tab: 'For teams', color: 'var(--color-duck)' },
	{ id: 'config', tab: 'Config', color: 'var(--color-sky)' },
	{ id: 'install', tab: 'Install', color: 'var(--color-beak)' }
] as const;

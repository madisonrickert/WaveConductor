import { FaVolumeUp, FaVolumeOff } from "react-icons/fa";

export interface VolumeButtonProps {
    volumeEnabled: boolean;
    onClick: () => void;
}

export const VolumeButton: React.FC<VolumeButtonProps> = ({ volumeEnabled, onClick }) => (
    <button className="user-volume" onClick={onClick}>
        {volumeEnabled ? <FaVolumeUp /> : <FaVolumeOff />}
    </button>
);
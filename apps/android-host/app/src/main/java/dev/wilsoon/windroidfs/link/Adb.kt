package dev.wilsoon.windroidfs.link

import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.os.BatteryManager
import android.provider.Settings

object Adb {
    /** The local socket `adb forward` reaches. Must match SOCKET in adb.rs. */
    const val SOCKET_NAME = "windroidfs"

    /** adbd runs as the shell user (root on debug builds). Anyone else is another app on this phone. */
    private val ADBD_UIDS = setOf(2000, 0)

    fun isAdbd(uid: Int) = uid in ADBD_UIDS

    /** USB debugging is on and a cable is in, so a PC may come through adb. */
    fun available(context: Context): Boolean {
        val debugging = Settings.Global.getInt(context.contentResolver, Settings.Global.ADB_ENABLED, 0) == 1
        val battery = context.registerReceiver(null, IntentFilter(Intent.ACTION_BATTERY_CHANGED))
        val onUsb = battery?.getIntExtra(BatteryManager.EXTRA_PLUGGED, 0) == BatteryManager.BATTERY_PLUGGED_USB
        return debugging && onUsb
    }
}

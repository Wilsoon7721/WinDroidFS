package dev.wilsoon.windroidfs.ui

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.wilsoon.windroidfs.ui.theme.WinDroidTheme

// A coloured circle used for status 
@Composable
fun StatusDot(color: Color, size: Dp = 10.dp) {
    Box(modifier = Modifier.size(size).clip(CircleShape).background(color))
}

// Section headings
@Composable
fun SectionLabel(text: String, modifier: Modifier = Modifier) {
    Text(
        text = text.uppercase(),
        color = WinDroidTheme.colors.textSecondary,
        fontSize = 12.sp,
        fontWeight = FontWeight.SemiBold,
        letterSpacing = 1.sp,
        modifier = modifier
    )
}

@Composable
fun WinDroidCard(
    modifier: Modifier = Modifier,
    onClick: (() -> Unit)? = null,
    content: @Composable ColumnScope.() -> Unit
) {
    Column(
        modifier = modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(16.dp))
            .background(WinDroidTheme.colors.surface)
            .then(if (onClick != null) Modifier.clickable(onClick = onClick) else Modifier)
            .padding(20.dp),
        content = content
    )
}

@Composable
fun ExpandableMethodCard(
    title: String,
    description: String,
    isExpanded: Boolean,
    onClick: () -> Unit,
    expandedContent: @Composable () -> Unit
) {
    WinDroidCard(onClick = onClick) {
        // Header
        Row(verticalAlignment = Alignment.CenterVertically) {
            StatusDot(
                color = if (isExpanded) WinDroidTheme.colors.brand else WinDroidTheme.colors.border,
                size = 8.dp
            )
            Spacer(modifier = Modifier.width(12.dp))
            Column {
                Text(
                    text = title,
                    color = WinDroidTheme.colors.textPrimary,
                    fontSize = 18.sp,
                    fontWeight = FontWeight.SemiBold
                )
                Text(
                    text = description,
                    color = WinDroidTheme.colors.textSecondary,
                    fontSize = 14.sp
                )
            }
        }

        // Expanding content area
        AnimatedVisibility(visible = isExpanded) {
            Column(modifier = Modifier.padding(top = 16.dp)) {
                HorizontalDivider(color = WinDroidTheme.colors.border, thickness = 1.dp)
                Spacer(modifier = Modifier.height(16.dp))
                expandedContent()
            }
        }
    }
}

// A numbered instruction
@Composable
fun SetupStep(number: Int, text: String) {
    Row(verticalAlignment = Alignment.Top) {
        Box(
            contentAlignment = Alignment.Center,
            modifier = Modifier
                .size(22.dp)
                .clip(CircleShape)
                .background(WinDroidTheme.colors.surfaceVariant)
        ) {
            Text(
                text = number.toString(),
                color = WinDroidTheme.colors.brand,
                fontSize = 12.sp,
                fontWeight = FontWeight.Bold
            )
        }
        Spacer(modifier = Modifier.width(12.dp))
        Text(
            text = text,
            color = WinDroidTheme.colors.textPrimary,
            fontSize = 14.sp,
            modifier = Modifier.padding(top = 2.dp)
        )
    }
}
